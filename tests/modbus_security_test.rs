//! e2e: standards-compliant Modbus Security (TLS 1.3 + CA-validated mTLS +
//! Role extension) round-trip from gateway to in-process mock and out via
//! MQTT. No device-api, no postgres — single hivemq container + in-process
//! rodbus TLS server + wiremock `/asyncapi` stub.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::{
    app,
    config::{Config, GatewayCredentials},
};
use fixtures::containers::start_hivemq;
use fixtures::modbus_security;
use fixtures::pki::gen_test_pki;
use fixtures::spec_stub::{build_spec_body, modbus_tls_binding, spawn_asyncapi_stub};
use futures::StreamExt;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder};
use serde_json::Value;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

/// Init tracing once across tests so gateway logs land in `--nocapture` output.
static TRACING_INIT: OnceLock<()> = OnceLock::new();
fn init_tracing() {
    TRACING_INIT.get_or_init(|| {
        let _ = tracing_subscriber::fmt::try_init();
    });
}

/// Site id used by both the test and the gateway when building MQTT topics.
const SITE_ID: &str = "site_001";
/// Device id in the DTM + MQTT topic.
const DEVICE_ID: &str = "meter_01";
/// Measurement name (template channel).
const MEASUREMENT: &str = "active_power";
/// Engineering unit terminal topic segment.
const UNIT: &str = "watts";
/// Bound on how long we wait for the gateway to publish its first sample.
const COLLECTION_TIMEOUT: Duration = Duration::from_secs(20);
/// Expected published value (canned register decode at addr 4000).
const EXPECTED_VALUE: f64 = 1_000_000.0;

#[tokio::test]
async fn gateway_polls_modbus_security_device_and_publishes_to_mqtt() -> Result<()> {
    init_tracing();
    // Arrange — PKI, hivemq, in-process Modbus Security server, spec stub.
    let subject_name = "meter-01.test.local";
    let pki = gen_test_pki(subject_name)?;
    let hivemq = start_hivemq().await?;
    let hivemq_port = hivemq.get_host_port_ipv4(1883).await?;
    let modbus = modbus_security::spawn(&pki).await?;
    let body = build_spec_body(
        DEVICE_ID,
        MEASUREMENT,
        UNIT,
        modbus_tls_binding(modbus.addr),
        subject_name,
    );
    let stub = spawn_asyncapi_stub(body).await;

    // MQTT subscriber on the expected topic — set up BEFORE the gateway
    // starts so we don't miss the first publish.
    let broker_url = format!("tcp://localhost:{hivemq_port}");
    let mut subscriber = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&broker_url)
            .client_id("modbus-security-test-sub")
            .finalize(),
    )?;
    let mut stream = subscriber.get_stream(64);
    subscriber
        .connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    let expected_topic =
        format!("sites/{SITE_ID}/devices/{DEVICE_ID}/measurements/{MEASUREMENT}/{UNIT}");
    subscriber.subscribe(&expected_topic, 0).await?;

    // Spawn the gateway with creds pointing at the PKI tempfiles.
    let cfg = Config {
        device_api_url: stub.uri(),
        broker_url: broker_url.clone(),
        site_id: SITE_ID.to_string(),
        log_level: "info".to_string(),
        gateway_credentials: Some(GatewayCredentials {
            ca_bundle_path: pki.ca_bundle.path().to_path_buf(),
            cert_path: pki.gateway_cert.path().to_path_buf(),
            key_path: pki.gateway_key.path().to_path_buf(),
        }),
    };
    let cancel = CancellationToken::new();
    let gateway_handle = {
        let cancel = cancel.clone();
        tokio::spawn(async move { app::run(cfg, cancel).await })
    };

    // Act — wait for the first published sample on the expected topic.
    let value = timeout(COLLECTION_TIMEOUT, async {
        loop {
            let Some(msg) = stream.next().await.flatten() else {
                continue;
            };
            if msg.topic() == expected_topic {
                let payload: Value = serde_json::from_slice(msg.payload())?;
                let value = payload["value"].as_f64().expect("non-numeric payload");
                return anyhow::Ok(value);
            }
        }
    })
    .await??;

    cancel.cancel();
    let _ = gateway_handle.await;

    // Assert — value matches the canned register decode (within float epsilon).
    assert!(
        (value - EXPECTED_VALUE).abs() < 1.0,
        "expected {EXPECTED_VALUE}, got {value}"
    );
    Ok(())
}
