//! e2e: SNMPv3 USM authPriv round-trip — gateway dials the mock-snmp-agent
//! configured for v3 USM, the snmp2 client discovers the agent's engine id,
//! does an authPriv GetRequest for the simulator's input_current OID, and
//! publishes to MQTT. Mirrors the other 4 protocol e2e tests; the only
//! cryptographic difference is HMAC-SHA-256 + AES-128 over UDP instead of
//! TLS over TCP.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{start_hivemq, start_mock_snmp_agent_v3};
use fixtures::spec_stub::spawn_asyncapi_stub;
use futures::StreamExt;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder};
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::time::Duration;
use testcontainers::core::ContainerPort;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

/// Init tracing once across tests so gateway logs land in `--nocapture` output.
static TRACING_INIT: OnceLock<()> = OnceLock::new();
fn init_tracing() {
    TRACING_INIT.get_or_init(|| {
        let _ = tracing_subscriber::fmt::try_init();
    });
}

const SITE_ID: &str = "site_001";
const DEVICE_ID: &str = "pdu_01";
const MEASUREMENT: &str = "input_current";
const UNIT: &str = "amps";
const SECURITY_NAME: &str = "gateway";
const AUTH_PASS: &str = "authpass1234";
const PRIV_PASS: &str = "privpass5678";
/// Simulator OID for input_current — matches the agent's `OID_INPUT_CURRENT`.
const TARGET_OID: &str = "1.3.6.1.4.1.1718.4.1.3.3.1.7";
/// Simulator sawtooth range — values fluctuate within `[100, 200]`.
const VALUE_RANGE: std::ops::RangeInclusive<f64> = 100.0..=200.0;
const COLLECTION_TIMEOUT: Duration = Duration::from_secs(45);

#[tokio::test]
async fn gateway_polls_snmpv3_usm_agent_and_publishes_to_mqtt() -> Result<()> {
    init_tracing();
    // Arrange — passphrases live in env vars matching the gateway's
    // load_usm_passphrases naming. Set BEFORE the gateway spawns.
    // SAFETY: integration_test isolation — this test binary is the only one
    // touching these vars, and it runs serially.
    unsafe {
        std::env::set_var("SNMP_USM_GATEWAY_AUTH_PASSPHRASE", AUTH_PASS);
        std::env::set_var("SNMP_USM_GATEWAY_PRIV_PASSPHRASE", PRIV_PASS);
    }

    let hivemq = start_hivemq().await?;
    let hivemq_port = hivemq.get_host_port_ipv4(1883).await?;
    let agent = start_mock_snmp_agent_v3(SECURITY_NAME, AUTH_PASS, PRIV_PASS).await?;
    let agent_host = agent.get_host().await?;
    let agent_port = agent.get_host_port_ipv4(ContainerPort::Udp(161)).await?;

    // Spec body: snmp binding + tls-equivalent USM trust block.
    let body = json!({
        "info": { "version": "v1" },
        "x-protocol-source": {
            DEVICE_ID: {
                MEASUREMENT: {
                    "unit": UNIT,
                    "poll_rate_hz": 1.0,
                    "protocol": "snmp",
                    "host": agent_host.to_string(),
                    "port": agent_port,
                    "oid": TARGET_OID,
                }
            }
        },
        "x-device-trust": {
            DEVICE_ID: {
                "trust_mode": "snmpv3_usm",
                "security_name": SECURITY_NAME,
                "auth_protocol": "sha256",
                "priv_protocol": "aes128",
            }
        }
    });
    let stub = spawn_asyncapi_stub(body).await;

    // MQTT subscriber on the expected topic before the gateway starts.
    let broker_url = format!("tcp://localhost:{hivemq_port}");
    let mut subscriber = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&broker_url)
            .client_id("snmp3-usm-test-sub")
            .finalize(),
    )?;
    let mut stream = subscriber.get_stream(64);
    subscriber
        .connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    let expected_topic =
        format!("sites/{SITE_ID}/devices/{DEVICE_ID}/measurements/{MEASUREMENT}/{UNIT}");
    subscriber.subscribe(&expected_topic, 0).await?;

    // Broker auth: set the env var the gateway loads. Tests use anonymous

    // hivemq so any value works; this just satisfies the env-required check.

    unsafe {
        std::env::set_var("MQTT_GATEWAY_PASSWORD", "test");
    }

    let cfg = Config {
        device_api_url: stub.uri(),
        broker_url: broker_url.clone(),
        mqtt_username: "arcnode_gateway".to_string(),
        site_id: SITE_ID.to_string(),
        log_level: "info".to_string(),
        // No PKI material for SNMP — USM is HMAC+symmetric, gated on env-var
        // passphrases. validate_trust_creds_alignment accepts Snmpv3Usm
        // without creds set.
        gateway_credentials: None,
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

    // Assert — published value lies in the simulator's sawtooth range.
    assert!(
        VALUE_RANGE.contains(&value),
        "expected value in {VALUE_RANGE:?}, got {value}"
    );
    Ok(())
}
