//! e2e: when a curtailment event ends, site distribution must hand the
//! modules back. Holding would leave each at its last event setpoint, so the
//! BESS keeps discharging into its reserve after the utility let go. Each
//! module goes back to its pre-event operator setpoint (0 if none).

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::modbus::client::{WordOrder, decode_int32, read_holding};
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{start_hivemq, start_mock_modbus_server_writable};
use fixtures::spec_stub::spawn_asyncapi_stub;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const SITE_ID: &str = "local_site";
const MODULES: [&str; 2] = ["module_1", "module_2"];
const RACKS: [&str; 2] = ["rack_1a", "rack_2a"];

fn rack_command(port: u16) -> Value {
    json!({
        "verb": "set", "target": "active_power", "unit": "watts",
        "protocol": "modbus_tcp", "host": "127.0.0.1", "port": port,
        "unit_id": "1", "address": 50, "scale": 1.0, "offset": 0.0,
    })
}

fn module_command(rack: &str) -> Value {
    json!({
        "verb": "set", "target": "active_power", "unit": "watts",
        "protocol": "distribute", "allocation_policy": "equal_split",
        "power_min": -4_000_000.0, "power_max": 4_000_000.0,
        "children": [{
            "device_id": rack,
            "operating_state_topic": format!("sites/{{site_id}}/devices/{rack}/measurements/operating_state/none"),
            "state_of_charge_topic": format!("sites/{{site_id}}/devices/{rack}/measurements/state_of_charge/percent"),
            "power_min": -4_000_000.0, "power_max": 4_000_000.0,
        }],
    })
}

async fn watts(port: u16) -> Result<i32> {
    Ok(decode_int32(
        &read_holding("127.0.0.1", port, 1, 50, 2).await?,
        WordOrder::HighLow,
    ))
}

/// Poll both racks until they hold `expected`, or fail after 15 s.
async fn wait_for(ports: [u16; 2], expected: [i32; 2], what: &str) -> Result<()> {
    timeout(Duration::from_secs(15), async {
        // A read error means a rack's setpoint register isn't written yet
        // (the mock serves it only after the first write): keep waiting.
        while [watts(ports[0]).await.ok(), watts(ports[1]).await.ok()]
            != [Some(expected[0]), Some(expected[1])]
        {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        anyhow::Ok(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("racks never reached {expected:?} ({what})"))?
}

async fn publish(op: &AsyncClient, topic: &str, payload: &str, qos: i32) -> Result<()> {
    op.publish(Message::new(
        format!("sites/{SITE_ID}/devices/{topic}"),
        payload,
        qos,
    ))
    .await?;
    Ok(())
}

#[tokio::test]
async fn ending_an_event_restores_each_modules_pre_event_setpoint() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    // Arrange — two modules, one rack each
    let network = fixtures::containers::unique_network();
    let hivemq = start_hivemq(&network).await?;
    let broker_url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let (r1, r2) = (
        start_mock_modbus_server_writable().await?,
        start_mock_modbus_server_writable().await?,
    );
    let ports = [
        r1.get_host_port_ipv4(502).await?,
        r2.get_host_port_ipv4(502).await?,
    ];
    let stub = spawn_asyncapi_stub(json!({
        "info": { "version": "v1" }, "x-protocol-source": {},
        "x-command-source": {
            MODULES[0]: { "set_active_power": module_command(RACKS[0]) },
            MODULES[1]: { "set_active_power": module_command(RACKS[1]) },
            RACKS[0]: { "set_active_power": rack_command(ports[0]) },
            RACKS[1]: { "set_active_power": rack_command(ports[1]) },
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
    fixtures::readiness::wait_for_gateway_ready(&broker_url, SITE_ID, &MODULES).await?;
    let op = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&broker_url)
            .client_id("release-op")
            .finalize(),
    )?;
    op.connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    for rack in RACKS {
        publish(
            &op,
            &format!("{rack}/measurements/operating_state/none"),
            r#"{"ts":"t","value":0}"#,
            0,
        )
        .await?;
        publish(
            &op,
            &format!("{rack}/measurements/state_of_charge/percent"),
            r#"{"ts":"t","value":50}"#,
            0,
        )
        .await?;
    }
    publish(
        &op,
        "module_1/measurements/state_of_charge/percent",
        r#"{"ts":"t","value":70}"#,
        0,
    )
    .await?;
    publish(
        &op,
        "module_2/measurements/state_of_charge/percent",
        r#"{"ts":"t","value":30}"#,
        0,
    )
    .await?;
    publish(
        &op,
        "der_dispatch/measurements/event_active/none",
        r#"{"ts":"t","value":false}"#,
        0,
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Before the event, the operator has module_1 charging at 100 kW.
    publish(
        &op,
        "module_1/commands/set/active_power/watts",
        r#"{"ts":"t","value":-100000,"command_id":"op-1"}"#,
        1,
    )
    .await?;
    wait_for(ports, [-100_000, 0], "operator's pre-event setpoint").await?;

    // Act 1 — a 400 kW curtailment: soc_weighted 70/30 across the modules.
    publish(
        &op,
        "der_dispatch/measurements/target_active_power/watts",
        r#"{"ts":"t","value":400000}"#,
        0,
    )
    .await?;
    publish(
        &op,
        "der_dispatch/measurements/target_setpoint_present/none",
        r#"{"ts":"t","value":true}"#,
        0,
    )
    .await?;
    publish(
        &op,
        "der_dispatch/measurements/event_active/none",
        r#"{"ts":"t","value":true}"#,
        0,
    )
    .await?;
    wait_for(ports, [280_000, 120_000], "event dispatch").await?;

    // Act 2 — the event ends. The retained target stays at 400 kW, as it
    // does in production, so only event_active tells the gateway to let go.
    publish(
        &op,
        "der_dispatch/measurements/event_active/none",
        r#"{"ts":"t","value":false}"#,
        0,
    )
    .await?;

    // Assert — module_1 back to charging, module_2 (no pre-event setpoint) to 0.
    wait_for(ports, [-100_000, 0], "release after the event ended").await?;

    cancel.cancel();
    gateway.await??;
    Ok(())
}
