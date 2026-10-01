//! Connection reuse. A gateway polling ~100 BMCs every second must not open a
//! new TCP connection per read: each closed one sits in TIME-WAIT for a
//! minute, and at that rate they exhaust the host's ephemeral ports.

use super::read_measurement;
use crate::asyncapi::types::RedfishBinding;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Minimal HTTP/1.1 keep-alive server: answers every request on a
/// connection with `{"v": 1.0}` and counts accepted connections.
async fn keep_alive_server() -> (u16, Arc<AtomicUsize>) {
    serve(r#"{"v": 1.0}"#).await
}

/// Same server, answering with `body`.
async fn serve(body: &'static str) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = accepted.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                );
                while let Ok(n) = sock.read(&mut buf).await {
                    if n == 0 || sock.write_all(resp.as_bytes()).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    (port, accepted)
}

#[tokio::test]
async fn repeated_reads_reuse_one_connection() {
    // Arrange
    let (port, accepted) = keep_alive_server().await;
    let binding = RedfishBinding {
        host: "127.0.0.1".to_string(),
        port,
        uri: "/Chassis/1/Power".to_string(),
        json_pointer: Some("/v".to_string()),
        scale: 1.0,
        value_map: None,
    };
    // Act — five polls, as one poll task would make over five ticks
    for _ in 0..5 {
        assert_eq!(read_measurement(&binding, None, None).await.unwrap(), 1.0);
    }
    // Assert
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn scale_converts_the_raw_reading_to_the_declared_unit() {
    // Arrange — NVIDIA's OperatingSpeedMHz is MHz; the measurement is hertz
    let (port, _) = keep_alive_server().await;
    let binding: RedfishBinding = serde_json::from_value(serde_json::json!({
        "host": "127.0.0.1", "port": port, "uri": "/Systems/1/Processors/GPU_0",
        "json_pointer": "/v", "scale": 1_000_000.0,
    }))
    .unwrap();
    // Act
    let hz = read_measurement(&binding, None, None).await.unwrap();
    // Assert
    assert_eq!(hz, 1_000_000.0);
}

#[test]
fn absent_scale_is_one() {
    // Specs from before the field existed must read exactly as before.
    let binding: RedfishBinding = serde_json::from_value(serde_json::json!({
        "host": "bmc", "port": 443, "uri": "/Chassis/1/Power", "json_pointer": null,
    }))
    .unwrap();
    assert_eq!(binding.scale, 1.0);
}

fn pump_state(port: u16, value_map: Option<serde_json::Value>) -> RedfishBinding {
    serde_json::from_value(serde_json::json!({
        "host": "127.0.0.1", "port": port, "uri": "/Chassis/CDU/Pumps",
        "json_pointer": "/Members/0/Status/State", "value_map": value_map,
    }))
    .unwrap()
}

#[tokio::test]
async fn a_text_reading_maps_to_its_number() {
    // Arrange — Redfish reports pump state as text
    let (port, _) = serve(r#"{"Members":[{"Status":{"State":"Enabled"}}]}"#).await;
    let map = serde_json::json!({ "Enabled": 1, "Disabled": 0, "UnavailableOffline": 2 });
    // Act
    let state = read_measurement(&pump_state(port, Some(map)), None, None)
        .await
        .unwrap();
    // Assert
    assert_eq!(state, 1.0);
}

#[tokio::test]
async fn a_text_reading_not_in_the_map_is_an_error() {
    // An unmapped state must not publish as some guessed number
    let (port, _) = serve(r#"{"Members":[{"Status":{"State":"Quiesced"}}]}"#).await;
    let map = serde_json::json!({ "Enabled": 1 });
    assert!(
        read_measurement(&pump_state(port, Some(map)), None, None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_text_reading_without_a_map_is_an_error() {
    let (port, _) = serve(r#"{"Members":[{"Status":{"State":"Enabled"}}]}"#).await;
    assert!(
        read_measurement(&pump_state(port, None), None, None)
            .await
            .is_err()
    );
}
