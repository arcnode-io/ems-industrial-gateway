//! e2e: control-driven mock — deterministic full-path assertion.
//!
//! The digital-twin drives mock-modbus-server registers via its HTTP
//! control surface (`PUT /registers`); the gateway polls real Modbus and
//! publishes engineering values. This test plays the twin's role and
//! asserts the EXACT commanded value lands on MQTT — no sawtooth-range
//! fuzziness — and that it survives fixture sim ticks (driven-skip).
//!
//! Requires the post-control-surface mock image (fixtures f77bf48+).

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{start_hivemq, start_mock_modbus_server};
use fixtures::spec_stub::{build_spec_body_plain, modbus_tls_binding, spawn_asyncapi_stub};
use futures::StreamExt;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

/// Init tracing once so gateway logs land in `--nocapture` output.
static TRACING_INIT: OnceLock<()> = OnceLock::new();
fn init_tracing() {
    TRACING_INIT.get_or_init(|| {
        let _ = tracing_subscriber::fmt::try_init();
    });
}

/// Site id used by both the test and the gateway when building MQTT topics.
const SITE_ID: &str = "site_001";
/// Device id in the spec + MQTT topic.
const DEVICE_ID: &str = "meter_01";
/// Measurement name (int32 at holding 4000-4001, scale 1.0).
const MEASUREMENT: &str = "kwh_delivered";
/// Engineering unit terminal topic segment.
const UNIT: &str = "watt_hours";
/// Bound on how long we wait for published samples.
const COLLECTION_TIMEOUT: Duration = Duration::from_secs(45);
/// Driven value: 2_345_678 = 0x0023CACE -> high 0x0023 (35), low 0xCACE (51918).
const DRIVEN_VALUE: f64 = 2_345_678.0;

#[tokio::test]
async fn control_driven_register_publishes_exact_engineering_value() -> Result<()> {
    init_tracing();
    // Arrange — hivemq + REAL mock-modbus container + /asyncapi stub.
    let hivemq = start_hivemq().await?;
    let hivemq_port = hivemq.get_host_port_ipv4(1883).await?;
    let mock = start_mock_modbus_server().await?;
    let modbus_port = mock.get_host_port_ipv4(502).await?;
    let control_port = mock.get_host_port_ipv4(8080).await?;
    let modbus_addr: SocketAddr = format!("127.0.0.1:{modbus_port}").parse()?;
    // Reason: no trust block — with tls_mutual declared and creds=None the
    // gateway fails fast at boot (validate_trust_creds_alignment).
    let body = build_spec_body_plain(
        DEVICE_ID,
        MEASUREMENT,
        UNIT,
        modbus_tls_binding(modbus_addr),
    );
    let stub = spawn_asyncapi_stub(body).await;

    let broker_url = format!("tcp://localhost:{hivemq_port}");
    let mut subscriber = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&broker_url)
            .client_id("modbus-control-test-sub")
            .finalize(),
    )?;
    let mut stream = subscriber.get_stream(64);
    subscriber
        .connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    let expected_topic =
        format!("sites/{SITE_ID}/devices/{DEVICE_ID}/measurements/{MEASUREMENT}/{UNIT}");
    subscriber.subscribe(&expected_topic, 0).await?;

    // Broker auth env — anonymous hivemq, any value satisfies the check.
    unsafe {
        std::env::set_var("MQTT_GATEWAY_PASSWORD", "test");
    }
    let cfg = Config {
        device_api_url: stub.uri(),
        broker_url: broker_url.clone(),
        mqtt_username: "arcnode_gateway".to_string(),
        site_id: SITE_ID.to_string(),
        log_level: "info".to_string(),
        gateway_credentials: None,
    };
    let cancel = CancellationToken::new();
    let gateway_handle = {
        let cancel = cancel.clone();
        tokio::spawn(async move { app::run(cfg, cancel).await })
    };

    // Sanity — first sample is the canned/sawtooth register decode.
    let baseline = next_value(&mut stream, &expected_topic).await?;
    assert!(
        (1_000_000.0..=1_010_000.0).contains(&baseline),
        "baseline should be in the sawtooth band, got {baseline}",
    );

    // Act — play the digital-twin: drive the register pair via control API.
    let control = format!("http://127.0.0.1:{control_port}/registers");
    let put = reqwest::Client::new()
        .put(&control)
        .json(&json!({"registers": {"4000": 35, "4001": 51918}}))
        .send()
        .await?;
    assert_eq!(put.status(), 204);

    // Assert — the EXACT driven value reaches MQTT...
    let driven = timeout(COLLECTION_TIMEOUT, async {
        loop {
            let value = next_value(&mut stream, &expected_topic).await?;
            if (value - DRIVEN_VALUE).abs() < f64::EPSILON {
                return anyhow::Ok(value);
            }
        }
    })
    .await??;
    assert_eq!(driven, DRIVEN_VALUE);

    // ...and survives further fixture sim ticks (driven-skip): the next
    // sample after ~1s of TICK_MS=1000 churn is still the driven value.
    let sustained = next_value(&mut stream, &expected_topic).await?;
    assert_eq!(
        sustained, DRIVEN_VALUE,
        "sawtooth must skip driven registers"
    );

    cancel.cancel();
    gateway_handle.await??;
    Ok(())
}

/// Pull the next payload value published on `topic`.
async fn next_value(
    stream: &mut paho_mqtt::AsyncReceiver<Option<paho_mqtt::Message>>,
    topic: &str,
) -> Result<f64> {
    timeout(COLLECTION_TIMEOUT, async {
        loop {
            let Some(msg) = stream.next().await.flatten() else {
                continue;
            };
            if msg.topic() == topic {
                let payload: Value = serde_json::from_slice(msg.payload())?;
                let value = payload["value"].as_f64().expect("non-numeric payload");
                return anyhow::Ok(value);
            }
        }
    })
    .await?
}
