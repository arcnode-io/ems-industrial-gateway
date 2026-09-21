//! e2e: Phase III site→module distribution. Proves der_dispatch's
//! target_active_power/event_active split across bess_module devices,
//! dispatched as real MQTT commands — each module's own existing
//! module→rack distribute machinery (already proven by distribute_test.rs)
//! then does its normal job underneath, with no der_dispatch-level schema
//! change anywhere.

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
const MODULE_1: &str = "module_1";
const MODULE_2: &str = "module_2";
const RACK_1: &str = "rack_1a";
const RACK_2: &str = "rack_2a";
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

fn module_command_entry(rack: &str) -> serde_json::Value {
    json!({
        "verb": "set", "target": "active_power", "unit": "watts",
        "protocol": "distribute",
        "allocation_policy": "equal_split",
        "power_min": -4_000_000.0,
        "power_max": 4_000_000.0,
        "children": [child_entry(rack)],
    })
}

async fn read_rack_watts(port: u16) -> Result<i32> {
    let words = read_holding("127.0.0.1", port, 1, REGISTER_ADDR, 2).await?;
    Ok(decode_int32(&words, WordOrder::HighLow))
}

#[tokio::test]
async fn site_target_splits_soc_weighted_across_modules_then_cascades_to_racks() -> Result<()> {
    init_tracing();
    // Arrange — hivemq + two writable mock-modbus racks (one per module).
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
            MODULE_1: { "set_active_power": module_command_entry(RACK_1) },
            MODULE_2: { "set_active_power": module_command_entry(RACK_2) },
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
            .client_id("site-dist-test-op")
            .finalize(),
    )?;
    let mut events = operator.get_stream(64);
    operator
        .connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    operator
        .subscribe(
            format!("sites/{SITE_ID}/devices/+/events/dispatch_state"),
            1,
        )
        .await?;

    // Arrange — each rack STANDBY (its own module's distribute needs this);
    // module_1 at 70% SoC, module_2 at 30% -> site split should be 70/30.
    for rack in [RACK_1, RACK_2] {
        operator
            .publish(Message::new(
                format!("sites/{SITE_ID}/devices/{rack}/measurements/operating_state/none"),
                r#"{"ts":"t","value":0}"#,
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
    operator
        .publish(Message::new(
            format!("sites/{SITE_ID}/devices/{MODULE_1}/measurements/state_of_charge/percent"),
            r#"{"ts":"t","value":70.0}"#,
            0,
        ))
        .await?;
    operator
        .publish(Message::new(
            format!("sites/{SITE_ID}/devices/{MODULE_2}/measurements/state_of_charge/percent"),
            r#"{"ts":"t","value":30.0}"#,
            0,
        ))
        .await?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Act — der_dispatch goes active with a 400kW site target. No command
    // topic involved: this is the reactive/tick trigger, not handle_command.
    operator
        .publish(Message::new(
            format!("sites/{SITE_ID}/devices/der_dispatch/measurements/event_active/none"),
            r#"{"ts":"t","value":true}"#,
            0,
        ))
        .await?;
    operator
        .publish(Message::new(
            format!("sites/{SITE_ID}/devices/der_dispatch/measurements/target_active_power/watts"),
            r#"{"ts":"t","value":400000.0}"#,
            0,
        ))
        .await?;

    // Assert — soc_weighted(70/30) of 400kW = 280k to module_1's rack,
    // 120k to module_2's rack, cascaded through each module's own
    // equal_split distribute (each module has exactly one rack, so its
    // full share lands there).
    let cascaded = timeout(Duration::from_secs(15), async {
        loop {
            let r1 = read_rack_watts(rack1_port).await?;
            let r2 = read_rack_watts(rack2_port).await?;
            if r1 == 280_000 && r2 == 120_000 {
                return anyhow::Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    assert!(
        cascaded.is_ok(),
        "site distribution never cascaded to the expected per-rack values"
    );

    // Both module-level commands should have completed the normal
    // received->done lifecycle, same as any operator command.
    let mut phases = Vec::new();
    while phases.len() < 4 {
        let msg = timeout(Duration::from_secs(5), events.next())
            .await?
            .flatten()
            .expect("dispatch_state stream closed early");
        let v: serde_json::Value = serde_json::from_slice(msg.payload())?;
        phases.push(v["phase"].as_str().unwrap_or_default().to_string());
    }
    assert!(phases.iter().filter(|p| *p == "done").count() >= 2);

    cancel.cancel();
    gateway_handle.await??;
    operator.disconnect(None).await?;
    Ok(())
}
