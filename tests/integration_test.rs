//! Integration: synthetic-channel e2e against real device-api + MQTT.
//!
//! Per-protocol dispatch is covered by the focused tests (modbus_security,
//! dnp3_tls, redfish_mtls, bacnet_sc, snmp3_usm). This file holds the
//! remaining flows that genuinely need the real device-api container:
//! synthetic-binding computation over cached MQTT inputs.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::{app, config::Config};
use std::sync::OnceLock;

static TRACING_INIT: OnceLock<()> = OnceLock::new();

/// Initialize tracing once across all tests so gateway logs land in the
/// test runner's output (visible with `--nocapture`). RUST_LOG honored.
fn init_tracing() {
    TRACING_INIT.get_or_init(|| {
        let _ = tracing_subscriber::fmt::try_init();
    });
}
use fixtures::containers::{start_device_api, start_hivemq, start_postgres};
use futures::StreamExt;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder};
use serde_json::Value;
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

/// Overall timeout for the publish-collection phase.
const COLLECTION_TIMEOUT: Duration = Duration::from_secs(45);

/// Synthetic-channel e2e (Phase 4 part A): no south-side device, just MQTT +
/// device-api + gateway. Test publishes the synthetic inputs to MQTT, gateway
/// caches + computes `import_limit − active_power`, publishes the result on
/// the canonical `bess_module_1.import_headroom` topic.
///
/// Will fail until the registry's `ems-device-api:latest` image picks up the
/// Phase 1 schema additions (Publisher.GATEWAY, SyntheticBinding, the
/// AsyncAPI `{device_id}` substitution). Auto-passes on the next CI cycle
/// after edp-api/ems-device-api commits land.
#[tokio::test]
async fn synthetic_headroom_publishes_subtract_of_cached_mqtt_inputs() -> Result<()> {
    init_tracing();
    // Arrange — minimal fixture: MQTT + postgres (device-api dep) + device-api
    let (pg, hivemq) = tokio::try_join!(start_postgres(), start_hivemq())?;
    let _ = &pg;
    let hivemq_port = hivemq.get_host_port_ipv4(1883).await?;
    let device_api = start_device_api().await?;
    let device_api_port = device_api.get_host_port_ipv4(3000).await?;

    // DTM has bess_module_1 + the synthetic measurement (seed_dtm.json).
    let dtm_template = include_str!("fixtures/seed_dtm.json");
    let dtm_body: Value = serde_json::from_str(dtm_template)?;
    let device_api_url = format!("http://localhost:{device_api_port}");
    let post_resp = reqwest::Client::new()
        .post(format!("{device_api_url}/topology"))
        .json(&dtm_body)
        .send()
        .await?;
    let status = post_resp.status();
    if status != 201 {
        let body = post_resp.text().await?;
        panic!("POST /topology failed: status={status} body={body}");
    }

    let broker_url = format!("tcp://localhost:{hivemq_port}");
    let create_sub = CreateOptionsBuilder::new()
        .server_uri(&broker_url)
        .client_id("synth-test-subscriber")
        .finalize();
    let mut sub = AsyncClient::new(create_sub)?;
    let mut stream = sub.get_stream(64);
    sub.connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    let output_topic =
        "sites/site_001/devices/bess_module_1/measurements/import_headroom/watts".to_string();
    sub.subscribe(&output_topic, 0).await?;

    // Spawn the gateway.
    let cfg = Config {
        device_api_url,
        broker_url: broker_url.clone(),
        site_id: "site_001".to_string(),
        log_level: "info".to_string(),
        gateway_credentials: None,
    };
    let cancel = CancellationToken::new();
    let gateway_handle = {
        let cancel = cancel.clone();
        tokio::spawn(async move { app::run(cfg, cancel).await })
    };

    // Act — publish synthetic inputs from a fresh client (gateway is the
    // subscriber-side; we play the role of the upstream producers).
    let create_pub = CreateOptionsBuilder::new()
        .server_uri(&broker_url)
        .client_id("synth-test-publisher")
        .finalize();
    let pub_client = AsyncClient::new(create_pub)?;
    pub_client
        .connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    // Small delay so the gateway has time to subscribe to the inputs.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let import_topic = "sites/site_001/devices/operating_envelope/measurements/import_limit/watts";
    let active_topic = "sites/site_001/devices/bess_module_1/measurements/active_power/watts";
    let import_payload = r#"{"ts":"2026-05-17T00:00:00Z","value":5000000.0}"#;
    let active_payload = r#"{"ts":"2026-05-17T00:00:00Z","value":2000000.0}"#;

    // Retain=true (per ADR-002 §11 measurement-family default) so the gateway
    // sees the inputs even if our publish lands before it subscribes.
    pub_client
        .publish(paho_mqtt::Message::new_retained(
            import_topic,
            import_payload,
            0,
        ))
        .await?;
    pub_client
        .publish(paho_mqtt::Message::new_retained(
            active_topic,
            active_payload,
            0,
        ))
        .await?;

    // Assert — collect at least one synthetic output sample within COLLECTION_TIMEOUT.
    let value = timeout(COLLECTION_TIMEOUT, async {
        loop {
            let Some(msg) = stream.next().await.flatten() else {
                continue;
            };
            if msg.topic() == output_topic {
                let payload: Value = serde_json::from_slice(msg.payload())?;
                let value = payload["value"].as_f64().expect("non-numeric payload");
                return anyhow::Ok(value);
            }
        }
    })
    .await??;

    cancel.cancel();
    gateway_handle.await??;
    pub_client.disconnect(None).await?;

    // 5_000_000 − 2_000_000 = 3_000_000 (loose epsilon for any float drift)
    assert!(
        (value - 3_000_000.0).abs() < 1.0,
        "synthetic headroom should be 3_000_000, got {value}",
    );

    Ok(())
}
