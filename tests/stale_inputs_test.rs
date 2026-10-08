//! e2e: the gateway doesn't steer what it can't see. A rack that stops
//! reporting gets 0 W and its share moves to the racks still reporting; a
//! POI meter that goes quiet ramps the module to 0 W instead of holding
//! its last output blind.

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
        "power_min": -500_000.0, "power_max": 500_000.0,
    })
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
                "poi_active_power_topic": "sites/{site_id}/devices/poi_meter/measurements/active_power/watts",
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

/// One second of readings from whichever `racks` still report, with the
/// POI meter reporting while `poi` is true. Site load 1 MW, wide envelope.
async fn second(feed: &AsyncClient, racks: &[&str], poi: bool) -> Result<()> {
    for rack in racks {
        publish(feed, rack, "operating_state/none", 2.0).await?;
        publish(feed, rack, "state_of_charge/percent", 60.0).await?;
    }
    publish(feed, MODULE_ID, "active_power/watts", 400_000.0).await?;
    publish(
        feed,
        "operating_envelope",
        "import_limit/watts",
        5_000_000.0,
    )
    .await?;
    publish(
        feed,
        "operating_envelope",
        "export_limit/watts",
        5_000_000.0,
    )
    .await?;
    if poi {
        publish(feed, "poi_meter", "active_power/watts", 600_000.0).await?;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    Ok(())
}

/// Feed seconds until the racks hold `watts`, or time out.
async fn until_racks_hold(
    feed: &AsyncClient,
    ports: [u16; 2],
    racks: &[&str],
    poi: bool,
    watts: (i32, i32),
) -> bool {
    timeout(Duration::from_secs(30), async {
        loop {
            second(feed, racks, poi).await?;
            // the writable mock refuses reads of a register nothing has written
            if let (Ok(r1), Ok(r2)) = (rack_watts(ports[0]).await, rack_watts(ports[1]).await)
                && (r1, r2) == watts
            {
                return anyhow::Ok(());
            }
        }
    })
    .await
    .is_ok()
}

async fn rack_watts(port: u16) -> Result<i32> {
    let words = read_holding("127.0.0.1", port, 1, REGISTER_ADDR, 2).await?;
    Ok(decode_int32(&words, WordOrder::HighLow))
}

#[tokio::test]
async fn racks_and_meters_gone_quiet_are_not_steered_blind() -> Result<()> {
    // Arrange — guarded module discharging 400 kW on the operator's command
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
            .client_id("stale-feed")
            .finalize(),
    )?;
    feed.connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    second(&feed, &RACKS, true).await?;
    let command = format!("sites/{SITE_ID}/devices/{MODULE_ID}/commands/set/active_power/watts");
    let payload = r#"{"ts":"t","value":400000,"command_id":"cmd-stale-1"}"#;
    feed.publish(Message::new(command, payload, 1)).await?;
    assert!(
        until_racks_hold(&feed, ports, &RACKS, true, (200_000, 200_000)).await,
        "never reached the commanded split"
    );

    // Act + Assert — rack_1 stops reporting: 0 W, rack_2 carries the 400 kW
    assert!(
        until_racks_hold(&feed, ports, &RACKS[1..], true, (0, 400_000)).await,
        "kept steering a rack it can't see"
    );
    // Act + Assert — the POI meter stops too: the module ramps to 0 W
    assert!(
        until_racks_hold(&feed, ports, &RACKS[1..], false, (0, 0)).await,
        "held its output with the POI meter quiet"
    );

    cancel.cancel();
    gateway.await??;
    Ok(())
}
