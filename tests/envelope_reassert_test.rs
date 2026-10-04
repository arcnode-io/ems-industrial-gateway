//! e2e: the envelope keeps its racks governed. A rack that reboots (or is
//! written by another master) holds something other than its share while
//! the share itself hasn't changed; the gateway must put it back.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::asyncapi::types::ModbusTcpBinding;
use ems_industrial_gateway::modbus::client::{
    ModbusDataType, WordOrder, decode_int32, read_holding, write_setpoint,
};
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
        "power_min": -500_000.0, "power_max": 500_000.0,
    })
}

fn rack_binding(port: u16) -> ModbusTcpBinding {
    ModbusTcpBinding {
        host: "127.0.0.1".to_string(),
        port,
        unit_id: "1".to_string(),
        address: REGISTER_ADDR,
        scale: 1.0,
        offset: 0.0,
        data_type: ModbusDataType::Int32,
        word_order: WordOrder::HighLow,
        function_code: None,
        scale_factor_address: None,
    }
}

fn spec(rack_ports: [u16; 2]) -> Value {
    let command = |port: u16| {
        json!({
            "verb": "set", "target": "active_power", "unit": "watts",
            "protocol": "modbus_tcp", "host": "127.0.0.1", "port": port,
            "unit_id": "1", "address": REGISTER_ADDR, "scale": 1.0, "offset": 0.0,
        })
    };
    json!({
        "info": { "version": "v1" },
        "x-protocol-source": {},
        "x-command-source": {
            MODULE_ID: { "set_active_power": {
                "verb": "set", "target": "active_power", "unit": "watts",
                "protocol": "distribute", "allocation_policy": "equal_split",
                "ramp_rate_per_sec": 0.1, "hysteresis_margin": 0.05, "hysteresis_dwell_secs": 30.0,
                "power_min": -1_000_000.0, "power_max": 1_000_000.0,
                "import_limit_topic": "sites/{site_id}/devices/operating_envelope/measurements/import_limit/watts",
                "export_limit_topic": "sites/{site_id}/devices/operating_envelope/measurements/export_limit/watts",
                "active_power_topic": format!("sites/{{site_id}}/devices/{MODULE_ID}/measurements/active_power/watts"),
                "children": [rack(RACKS[0]), rack(RACKS[1])],
            } },
            RACKS[0]: { "set_active_power": command(rack_ports[0]) },
            RACKS[1]: { "set_active_power": command(rack_ports[1]) },
        },
    })
}

async fn publish(feed: &AsyncClient, device: &str, measurement: &str, value: f64) -> Result<()> {
    let topic = format!("sites/{SITE_ID}/devices/{device}/measurements/{measurement}");
    let payload = json!({ "ts": "t", "value": value }).to_string();
    feed.publish(Message::new(topic, payload, 0)).await?;
    Ok(())
}

/// One second of readings for an idle battery inside an open envelope.
async fn second(feed: &AsyncClient) -> Result<()> {
    for rack in RACKS {
        publish(feed, rack, "operating_state/none", 0.0).await?;
        publish(feed, rack, "state_of_charge/percent", 60.0).await?;
    }
    publish(feed, MODULE_ID, "active_power/watts", 0.0).await?;
    publish(
        feed,
        "operating_envelope",
        "import_limit/watts",
        2_000_000.0,
    )
    .await?;
    publish(feed, "operating_envelope", "export_limit/watts", 0.0).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    Ok(())
}

async fn rack_watts(port: u16) -> Result<i32> {
    let words = read_holding("127.0.0.1", port, 1, REGISTER_ADDR, 2).await?;
    Ok(decode_int32(&words, WordOrder::HighLow))
}

#[tokio::test]
async fn a_rack_that_drops_its_setpoint_is_put_back() -> Result<()> {
    // Arrange — an idle guarded battery, its racks commanded to 0 W
    let network = unique_network();
    let hivemq = start_hivemq(&network).await?;
    let broker_url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let rack1 = start_mock_modbus_server_writable().await?;
    let rack2 = start_mock_modbus_server_writable().await?;
    let ports = [
        rack1.get_host_port_ipv4(502).await?,
        rack2.get_host_port_ipv4(502).await?,
    ];
    let stub = spawn_asyncapi_stub(spec(ports)).await;
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
    let feed = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&broker_url)
            .client_id("reassert-feed")
            .finalize(),
    )?;
    feed.connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    for _ in 0..3 {
        second(&feed).await?;
    }

    // Act — rack_1 reboots into its 1.08 MW default
    write_setpoint(&rack_binding(ports[0]), 1_080_000.0, None, None).await?;
    let restored = timeout(Duration::from_secs(10), async {
        loop {
            second(&feed).await?;
            if rack_watts(ports[0]).await? == 0 {
                return anyhow::Ok(());
            }
        }
    })
    .await;

    // Assert — the gateway wrote its share back
    assert!(restored.is_ok(), "rack_1 left free-running at its default");

    cancel.cancel();
    gateway.await??;
    Ok(())
}
