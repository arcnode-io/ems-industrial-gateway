//! e2e: a rack's live power limit (`max_discharge_power` on the broker)
//! caps its share of a distribute command; the other rack takes the rest.
//! Contracts only: spec stub (device-api), broker topics, mock Modbus racks.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::modbus::client::{WordOrder, decode_int32, read_holding};
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{start_hivemq, start_mock_modbus_server_writable, unique_network};
use fixtures::spec_stub::spawn_asyncapi_stub;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const SITE_ID: &str = "local_site";
const MODULE_ID: &str = "bess_module_1";
const RACKS: [&str; 2] = ["rack_1", "rack_2"];
const REGISTER_ADDR: u16 = 50;

fn rack(device_id: &str) -> Value {
    json!({
        "device_id": device_id,
        "operating_state_topic": format!("sites/{{site_id}}/devices/{device_id}/measurements/operating_state/none"),
        "state_of_charge_topic": format!("sites/{{site_id}}/devices/{device_id}/measurements/state_of_charge/percent"),
        "power_min": -1_927_000.0, "power_max": 1_927_000.0,
    })
}

fn rack_command(port: u16) -> Value {
    json!({
        "verb": "set", "target": "active_power", "unit": "watts",
        "protocol": "modbus_tcp", "host": "127.0.0.1", "port": port,
        "unit_id": "1", "address": REGISTER_ADDR, "scale": 1.0, "offset": 0.0,
    })
}

async fn publish(op: &AsyncClient, device: &str, measurement: &str, value: f64) -> Result<()> {
    let topic = format!("sites/{SITE_ID}/devices/{device}/measurements/{measurement}");
    let payload = json!({ "ts": "t", "value": value }).to_string();
    op.publish(Message::new(topic, payload, 0)).await?;
    Ok(())
}

async fn rack_watts(port: u16) -> Result<i32> {
    let words = read_holding("127.0.0.1", port, 1, REGISTER_ADDR, 2).await?;
    Ok(decode_int32(&words, WordOrder::HighLow))
}

#[tokio::test]
async fn a_derated_rack_takes_only_what_its_limit_allows() -> Result<()> {
    // Arrange — a plain distribute module over two 1,927 kW racks
    let network = unique_network();
    let hivemq = start_hivemq(&network).await?;
    let broker_url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let rack1 = start_mock_modbus_server_writable().await?;
    let rack2 = start_mock_modbus_server_writable().await?;
    let ports = [
        rack1.get_host_port_ipv4(502).await?,
        rack2.get_host_port_ipv4(502).await?,
    ];
    let stub = spawn_asyncapi_stub(json!({
        "info": { "version": "v1" },
        "x-protocol-source": {},
        "x-command-source": {
            MODULE_ID: { "set_active_power": {
                "verb": "set", "target": "active_power", "unit": "watts",
                "protocol": "distribute", "allocation_policy": "equal_split",
                "children": [rack(RACKS[0]), rack(RACKS[1])],
            } },
            RACKS[0]: { "set_active_power": rack_command(ports[0]) },
            RACKS[1]: { "set_active_power": rack_command(ports[1]) },
        },
    }))
    .await;
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
    let gateway = {
        let cancel = cancel.clone();
        tokio::spawn(async move { app::run(cfg, cancel).await })
    };
    fixtures::readiness::wait_for_gateway_ready(&broker_url, SITE_ID, &[MODULE_ID]).await?;
    let op = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&broker_url)
            .client_id("rack-limits-op")
            .finalize(),
    )?;
    op.connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    for rack in RACKS {
        publish(&op, rack, "operating_state/none", 0.0).await?;
        publish(&op, rack, "state_of_charge/percent", 50.0).await?;
    }
    // rack_1 near empty: 10% SoC on the template's curve is 963.5 kW
    publish(&op, RACKS[0], "max_discharge_power/watts", 963_500.0).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Act — 2 MW: equal split would ask 1 MW of each
    let command = format!("sites/{SITE_ID}/devices/{MODULE_ID}/commands/set/active_power/watts");
    let payload = r#"{"ts":"t","value":2000000,"command_id":"cmd-limits-1"}"#;
    op.publish(Message::new(command, payload, 1)).await?;
    let split = timeout(Duration::from_secs(10), async {
        loop {
            // the writable mock refuses reads of a register nothing has written
            if let (Ok(r1), Ok(r2)) = (rack_watts(ports[0]).await, rack_watts(ports[1]).await) {
                return anyhow::Ok((r1, r2));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await??;

    // Assert — rack_1 held to its limit, rack_2 picks up the remainder
    assert_eq!(split, (963_500, 1_036_500));

    cancel.cancel();
    gateway.await??;
    Ok(())
}
