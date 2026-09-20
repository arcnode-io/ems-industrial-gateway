//! e2e: Phase III rebalance-on-drift. Proves the genuinely new behavior — a
//! plain (unguarded) distribute binding's child split rebalances on its own
//! tick when a child's SoC drifts, with NO new command message involved.
//! The reactive command->write path is already covered by distribute_test.rs;
//! this proves the periodic side of the hybrid trigger design.

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
const OPERATING_STATE_STANDBY: f64 = 0.0;
const REGISTER_ADDR: u16 = 50;

fn rack_command_entry(addr: SocketAddr) -> serde_json::Value {
    json!({
        "verb": "set", "target": "active_power", "unit": "watts",
        "protocol": "modbus_tcp",
        "host": addr.ip().to_string(), "port": addr.port(), "unit_id": "1",
        "address": REGISTER_ADDR, "scale": 1.0, "offset": 0.0,
    })
}

fn child_entry(device_id: &str) -> serde_json::Value {
    json!({
        "device_id": device_id,
        "operating_state_topic": format!("sites/{{site_id}}/devices/{device_id}/measurements/operating_state/none"),
        "state_of_charge_topic": format!("sites/{{site_id}}/devices/{device_id}/measurements/state_of_charge/percent"),
        "power_min": -4_000_000.0,
        "power_max": 4_000_000.0,
    })
}

async fn read_rack_watts(port: u16) -> Result<i32> {
    let words = read_holding("127.0.0.1", port, 1, REGISTER_ADDR, 2).await?;
    Ok(decode_int32(&words, WordOrder::HighLow))
}

#[tokio::test]
async fn plain_distribute_rebalances_on_soc_drift_with_no_new_command() -> Result<()> {
    init_tracing();
    // Arrange — hivemq + two real writable mock-modbus racks, a PLAIN
    // (unguarded) soc_weighted distribute binding — no ramp_rate_per_sec/
    // hysteresis fields, so envelope_guard_config would return None.
    let network = fixtures::containers::unique_network();
    let hivemq = start_hivemq(&network).await?;
    let hivemq_port = hivemq.get_host_port_ipv4(1883).await?;
    let rack1 = start_mock_modbus_server_writable().await?;
    let rack1_port = rack1.get_host_port_ipv4(502).await?;
    let rack1_addr: SocketAddr = format!("127.0.0.1:{rack1_port}").parse().unwrap();
    let rack2 = start_mock_modbus_server_writable().await?;
    let rack2_port = rack2.get_host_port_ipv4(502).await?;
    let rack2_addr: SocketAddr = format!("127.0.0.1:{rack2_port}").parse().unwrap();

    let body = json!({
        "info": { "version": "v1" },
        "x-protocol-source": {},
        "x-command-source": {
            MODULE_ID: {
                "set_active_power": {
                    "verb": "set", "target": "active_power", "unit": "watts",
                    "protocol": "distribute",
                    "allocation_policy": "soc_weighted",
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
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut operator = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&broker_url)
            .client_id("rebalance-test-op")
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

    // Arrange — both racks at equal SoC (50%) so the initial command splits evenly.
    for rack in [RACK_1, RACK_2] {
        operator
            .publish(Message::new(
                format!("sites/{SITE_ID}/devices/{rack}/measurements/operating_state/none"),
                format!(r#"{{"ts":"t","value":{OPERATING_STATE_STANDBY}}}"#),
                0,
            ))
            .await?;
        operator
            .publish(Message::new(
                format!("sites/{SITE_ID}/devices/{rack}/measurements/state_of_charge/percent"),
                r#"{"ts":"t","value":50.0}"#,
                0,
            ))
            .await?;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Act — one real command: 200kW, equal SoC -> 100kW/rack.
    operator
        .publish(Message::new(
            format!("sites/{SITE_ID}/devices/{MODULE_ID}/commands/set/active_power/watts"),
            r#"{"ts":"t","value":200000,"command_id":"cmd-1"}"#,
            1,
        ))
        .await?;
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
    assert_eq!(read_rack_watts(rack1_port).await?, 100_000);
    assert_eq!(read_rack_watts(rack2_port).await?, 100_000);

    // Act — NO new command. Drift rack_1 to 80% SoC, rack_2 to 20%. The
    // module-level target (200kW) never changes, only the split should.
    operator
        .publish(Message::new(
            format!("sites/{SITE_ID}/devices/{RACK_1}/measurements/state_of_charge/percent"),
            r#"{"ts":"t","value":80.0}"#,
            0,
        ))
        .await?;
    operator
        .publish(Message::new(
            format!("sites/{SITE_ID}/devices/{RACK_2}/measurements/state_of_charge/percent"),
            r#"{"ts":"t","value":20.0}"#,
            0,
        ))
        .await?;

    // Assert — soc_weighted(80/20) of 200kW = 160k/40k, written autonomously.
    let rebalanced = timeout(Duration::from_secs(10), async {
        loop {
            let r1 = read_rack_watts(rack1_port).await?;
            let r2 = read_rack_watts(rack2_port).await?;
            if r1 == 160_000 && r2 == 40_000 {
                return anyhow::Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    assert!(
        rebalanced.is_ok(),
        "distribute task never rebalanced to the new SoC-weighted split"
    );

    cancel.cancel();
    gateway_handle.await??;
    operator.disconnect(None).await?;
    Ok(())
}
