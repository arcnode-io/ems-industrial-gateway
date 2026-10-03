//! e2e: the operator's reserve (`der_dispatch/operator_reserve`) holds the
//! battery back from the envelope. High-risk both ways: discharging into a
//! reserve spends what the operator kept back; a reserve that never lets go
//! leaves compute shed for nothing.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::modbus::client::{WordOrder, decode_int32, read_holding};
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{start_hivemq, start_mock_modbus_server_writable};
use fixtures::spec_stub::spawn_asyncapi_stub;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const SITE_ID: &str = "local_site";
const MODULE_ID: &str = "bess_module_1";
const METER_ID: &str = "meter_01";
const RACKS: [&str; 2] = ["rack_1", "rack_2"];
const REGISTER_ADDR: u16 = 50;

fn rack_command(addr: SocketAddr) -> Value {
    json!({
        "verb": "set", "target": "active_power", "unit": "watts",
        "protocol": "modbus_tcp", "host": addr.ip().to_string(), "port": addr.port(),
        "unit_id": "1", "address": REGISTER_ADDR, "scale": 1.0, "offset": 0.0,
    })
}

fn child(device_id: &str) -> Value {
    json!({
        "device_id": device_id,
        "operating_state_topic": format!("sites/{{site_id}}/devices/{device_id}/measurements/operating_state/none"),
        "state_of_charge_topic": format!("sites/{{site_id}}/devices/{device_id}/measurements/state_of_charge/percent"),
        "power_min": -4_000_000.0,
        "power_max": 4_000_000.0,
    })
}

async fn rack_watts(port: u16) -> Result<i32> {
    let words = read_holding("127.0.0.1", port, 1, REGISTER_ADDR, 2).await?;
    Ok(decode_int32(&words, WordOrder::HighLow))
}

async fn publish(op: &AsyncClient, device: &str, measurement: &str, value: f64) -> Result<()> {
    op.publish(Message::new(
        format!("sites/{SITE_ID}/devices/{device}/measurements/{measurement}"),
        format!(r#"{{"ts":"t","value":{value}}}"#),
        0,
    ))
    .await?;
    Ok(())
}

#[tokio::test]
async fn a_reserve_above_the_stored_energy_keeps_the_battery_out() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    // Arrange — envelope-guarded module over two writable racks, a zero
    // import envelope and a site importing 300 kW through the POI
    let network = fixtures::containers::unique_network();
    let hivemq = start_hivemq(&network).await?;
    let broker_url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let rack1 = start_mock_modbus_server_writable().await?;
    let rack2 = start_mock_modbus_server_writable().await?;
    let port1 = rack1.get_host_port_ipv4(502).await?;
    let port2 = rack2.get_host_port_ipv4(502).await?;
    let addr = |p: u16| -> SocketAddr { format!("127.0.0.1:{p}").parse().unwrap() };
    let stub = spawn_asyncapi_stub(json!({
        "info": { "version": "v1" },
        "x-protocol-source": {},
        "x-command-source": {
            MODULE_ID: { "set_active_power": {
                "verb": "set", "target": "active_power", "unit": "watts",
                "protocol": "distribute", "allocation_policy": "equal_split",
                "ramp_rate_per_sec": 0.10, "hysteresis_margin": 0.05, "hysteresis_dwell_secs": 30.0,
                "power_min": -4_000_000.0, "power_max": 4_000_000.0,
                "import_limit_topic": "sites/{site_id}/devices/operating_envelope/measurements/import_limit/watts",
                "export_limit_topic": "sites/{site_id}/devices/operating_envelope/measurements/export_limit/watts",
                "active_power_topic": format!("sites/{{site_id}}/devices/{MODULE_ID}/measurements/active_power/watts"),
                "poi_active_power_topic": format!("sites/{{site_id}}/devices/{METER_ID}/measurements/active_power/watts"),
                "operator_reserve_topic": "sites/{site_id}/devices/der_dispatch/measurements/operator_reserve/watt_hours",
                "site_capacity_wh": 8_000_000.0,
                "children": [child(RACKS[0]), child(RACKS[1])],
            }},
            RACKS[0]: { "set_active_power": rack_command(addr(port1)) },
            RACKS[1]: { "set_active_power": rack_command(addr(port2)) },
        }
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
            .client_id("operator-reserve-op")
            .finalize(),
    )?;
    op.connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    for rack in RACKS {
        publish(&op, rack, "operating_state/none", 0.0).await?;
        publish(&op, rack, "state_of_charge/percent", 50.0).await?;
    }
    publish(&op, "operating_envelope", "import_limit/watts", 0.0).await?;
    publish(&op, "operating_envelope", "export_limit/watts", 0.0).await?;
    publish(&op, MODULE_ID, "active_power/watts", 0.0).await?;
    let reserve = |wh: f64| {
        Message::new_retained(
            format!(
                "sites/{SITE_ID}/devices/der_dispatch/measurements/operator_reserve/watt_hours"
            ),
            format!(r#"{{"ts":"t","value":{wh}}}"#),
            1,
        )
    };

    // Act — keep 6 of 8 MWh (75%) while the racks sit at 50% and the POI
    // imports 300 kW
    op.publish(reserve(6_000_000.0)).await?;
    for _ in 0..12 {
        publish(&op, METER_ID, "active_power/watts", 300_000.0).await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    // Assert — the battery never discharged into the reserve
    assert!(
        rack_watts(port1).await? <= 0,
        "rack_1 discharged into the reserve"
    );
    assert!(
        rack_watts(port2).await? <= 0,
        "rack_2 discharged into the reserve"
    );

    // Act — the operator releases the reserve
    op.publish(reserve(0.0)).await?;
    let discharging = timeout(Duration::from_secs(15), async {
        loop {
            publish(&op, METER_ID, "active_power/watts", 300_000.0).await?;
            if rack_watts(port1).await? > 0 && rack_watts(port2).await? > 0 {
                return anyhow::Ok(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await;
    assert!(
        discharging.is_ok(),
        "battery never resumed once the reserve was released"
    );

    cancel.cancel();
    gateway.await??;
    Ok(())
}
