//! e2e: bess_module distribute round-trip. One command in on
//! `bess_module_1`, N real Modbus writes out to its rack children — proves
//! the full pipeline (cache reads → max-min fair allocation → per-child
//! write), not just the allocation math in isolation.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::modbus::client::{WordOrder, decode_int32, read_holding};
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{start_hivemq, start_mock_modbus_server_writable};
use fixtures::spec_stub::spawn_asyncapi_stub;
use futures::stream::StreamExt;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde_json::json;
use std::net::SocketAddr;
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
const MODULE_ID: &str = "bess_module_1";
const RACK_1: &str = "rack_1";
const RACK_2: &str = "rack_2";
/// STANDBY per bess_rack.yaml's operating_state enum (0=STANDBY).
const OPERATING_STATE_STANDBY: f64 = 0.0;
/// Write-target register address, matching bess_rack.yaml's set_active_power.
const REGISTER_ADDR: u16 = 50;

/// The rack's own x-command-source entry: a real Modbus write binding.
fn rack_command_entry(addr: SocketAddr) -> serde_json::Value {
    json!({
        "verb": "set",
        "target": "active_power",
        "unit": "watts",
        "protocol": "modbus_tcp",
        "host": addr.ip().to_string(),
        "port": addr.port(),
        "unit_id": "1",
        "address": REGISTER_ADDR,
        "scale": 1.0,
        "offset": 0.0,
    })
}

/// One child entry in the module's distribute binding.
fn child_entry(device_id: &str) -> serde_json::Value {
    json!({
        "device_id": device_id,
        "operating_state_topic": format!("sites/{{site_id}}/devices/{device_id}/measurements/operating_state/none"),
        "state_of_charge_topic": format!("sites/{{site_id}}/devices/{device_id}/measurements/state_of_charge/percent"),
        "power_min": -4_000_000.0,
        "power_max": 4_000_000.0,
    })
}

#[tokio::test]
async fn distribute_command_writes_equal_split_to_both_racks() -> Result<()> {
    init_tracing();
    // Arrange — hivemq + two real writable mock-modbus containers (racks).
    let network = fixtures::containers::unique_network();
    let hivemq = start_hivemq(&network).await?;
    let hivemq_port = hivemq.get_host_port_ipv4(1883).await?;
    let rack1 = start_mock_modbus_server_writable().await?;
    let rack1_addr: SocketAddr = format!("127.0.0.1:{}", rack1.get_host_port_ipv4(502).await?)
        .parse()
        .unwrap();
    let rack2 = start_mock_modbus_server_writable().await?;
    let rack2_addr: SocketAddr = format!("127.0.0.1:{}", rack2.get_host_port_ipv4(502).await?)
        .parse()
        .unwrap();

    let body = json!({
        "info": { "version": "v1" },
        "x-protocol-source": {},
        "x-command-source": {
            MODULE_ID: {
                "set_active_power": {
                    "verb": "set",
                    "target": "active_power",
                    "unit": "watts",
                    "protocol": "distribute",
                    "allocation_policy": "equal_split",
                    "children": [child_entry(RACK_1), child_entry(RACK_2)],
                }
            },
            RACK_1: { "set_active_power": rack_command_entry(rack1_addr) },
            RACK_2: { "set_active_power": rack_command_entry(rack2_addr) },
        }
    });
    let stub = spawn_asyncapi_stub(body).await;

    let broker_url = format!("tcp://localhost:{hivemq_port}");
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

    // Give the gateway a moment to fetch the spec + establish subscriptions
    // before the operator publishes anything.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut operator = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&broker_url)
            .client_id("distribute-test-op")
            .finalize(),
    )?;
    let mut events = operator.get_stream(64);
    operator
        .connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    operator
        .subscribe(
            format!("sites/{SITE_ID}/devices/{MODULE_ID}/events/dispatch_state"),
            1,
        )
        .await?;

    // Arrange — seed both racks' cached operating_state + state_of_charge
    // (normally landed by the regular poll loop; published directly here).
    for rack in [RACK_1, RACK_2] {
        operator
            .publish(Message::new(
                format!("sites/{SITE_ID}/devices/{rack}/measurements/operating_state/none"),
                format!(r#"{{"ts":"2026-07-03T00:00:00Z","value":{OPERATING_STATE_STANDBY}}}"#),
                0,
            ))
            .await?;
        operator
            .publish(Message::new(
                format!("sites/{SITE_ID}/devices/{rack}/measurements/state_of_charge/percent"),
                r#"{"ts":"2026-07-03T00:00:00Z","value":50.0}"#,
                0,
            ))
            .await?;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Act — dispatch the module setpoint: 200kW split equal_split -> 100kW/rack.
    operator
        .publish(Message::new(
            format!("sites/{SITE_ID}/devices/{MODULE_ID}/commands/set/active_power/watts"),
            r#"{"ts":"2026-07-03T00:00:00Z","value":200000,"command_id":"cmd-dist-1"}"#,
            1,
        ))
        .await?;

    // Assert — received -> done.
    let mut phases = Vec::new();
    while phases.len() < 2 {
        let msg = timeout(Duration::from_secs(10), events.next())
            .await?
            .flatten()
            .expect("dispatch_state stream closed early");
        let v: serde_json::Value = serde_json::from_slice(msg.payload())?;
        phases.push(v["phase"].as_str().unwrap_or_default().to_string());
    }
    assert_eq!(phases, vec!["received", "done"]);

    // Assert — each rack's register actually holds its 100kW share.
    for addr in [rack1_addr, rack2_addr] {
        let words = read_holding("127.0.0.1", addr.port(), 1, REGISTER_ADDR, 2).await?;
        let raw = decode_int32(&words, WordOrder::HighLow);
        assert_eq!(
            raw, 100_000,
            "rack at {addr} should hold its equal_split share"
        );
    }

    cancel.cancel();
    gateway_handle.await??;
    operator.disconnect(None).await?;
    Ok(())
}
