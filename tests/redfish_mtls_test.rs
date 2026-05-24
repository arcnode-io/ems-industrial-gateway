//! e2e: Redfish HTTPS+mTLS (DSP0266 §13.1 + §13.3.5) round-trip from gateway
//! to in-process axum-rustls service and out via MQTT. 1 hivemq container +
//! in-process Redfish service + wiremock `/asyncapi`.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::{
    app,
    config::{Config, GatewayCredentials},
};
use fixtures::containers::start_hivemq;
use fixtures::pki::gen_test_pki;
use fixtures::redfish_security;
use fixtures::spec_stub::{build_spec_body, redfish_tls_binding, spawn_asyncapi_stub};
use futures::StreamExt;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder};
use serde_json::Value;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

static TRACING_INIT: OnceLock<()> = OnceLock::new();
fn init_tracing() {
    TRACING_INIT.get_or_init(|| {
        let _ = tracing_subscriber::fmt::try_init();
    });
}

const SITE_ID: &str = "site_001";
const DEVICE_ID: &str = "switch_01";
const MEASUREMENT: &str = "inlet_temp";
const UNIT: &str = "celsius";
const COLLECTION_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test]
async fn gateway_polls_redfish_mtls_service_and_publishes_to_mqtt() -> Result<()> {
    init_tracing();
    // Arrange — PKI, hivemq, in-process Redfish HTTPS+mTLS service, spec stub.
    let subject_name = "switch-01.test.local";
    let pki = gen_test_pki(subject_name)?;
    let hivemq = start_hivemq().await?;
    let hivemq_port = hivemq.get_host_port_ipv4(1883).await?;
    let redfish = redfish_security::spawn(&pki).await?;
    let body = build_spec_body(
        DEVICE_ID,
        MEASUREMENT,
        UNIT,
        redfish_tls_binding(
            redfish.addr,
            redfish_security::RESOURCE_PATH,
            redfish_security::JSON_POINTER,
        ),
        subject_name,
    );
    let stub = spawn_asyncapi_stub(body).await;

    let broker_url = format!("tcp://localhost:{hivemq_port}");
    let mut subscriber = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&broker_url)
            .client_id("redfish-mtls-test-sub")
            .finalize(),
    )?;
    let mut stream = subscriber.get_stream(64);
    subscriber
        .connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    let expected_topic =
        format!("sites/{SITE_ID}/devices/{DEVICE_ID}/measurements/{MEASUREMENT}/{UNIT}");
    subscriber.subscribe(&expected_topic, 0).await?;

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

    // Act — wait for the first published sample.
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

    // Assert — matches the static reading the in-process service returns.
    assert!(
        (value - redfish_security::TEMPERATURE).abs() < 0.01,
        "expected {}, got {value}",
        redfish_security::TEMPERATURE
    );
    Ok(())
}
