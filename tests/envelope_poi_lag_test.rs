//! e2e: a lagging POI meter must not make the envelope ring. Site load is
//! meter + battery, and the meter's reading trails the battery's by 1–2 s.
//! Unfiltered, a battery step is briefly counted as load and the target
//! doubles, exporting the whole site load. The load estimate is rate-limited
//! to a fraction of the pack's rating, so the step can't reach it.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::modbus::client::{WordOrder, decode_int32, read_holding};
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{start_hivemq, start_mock_modbus_server_writable};
use fixtures::spec_stub::spawn_asyncapi_stub;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const SITE_ID: &str = "site_001";
const MODULE_ID: &str = "bess_module_1";
const METER_ID: &str = "meter_01";
const RACKS: [&str; 2] = ["rack_1", "rack_2"];
const LOAD_W: f64 = 1_120_000.0;
/// 1%/s of the binding's 8 MW power_max.
const RAMP_W_PER_S: f64 = 80_000.0;

fn rack_command(port: u16) -> Value {
    json!({
        "verb": "set", "target": "active_power", "unit": "watts",
        "protocol": "modbus_tcp", "host": "127.0.0.1", "port": port,
        "unit_id": "1", "address": 50, "scale": 1.0, "offset": 0.0,
    })
}

fn child(id: &str) -> Value {
    json!({
        "device_id": id,
        "operating_state_topic": format!("sites/{{site_id}}/devices/{id}/measurements/operating_state/none"),
        "state_of_charge_topic": format!("sites/{{site_id}}/devices/{id}/measurements/state_of_charge/percent"),
        "power_min": -4_000_000.0, "power_max": 4_000_000.0,
    })
}

async fn racks_total(ports: [u16; 2]) -> Result<i64> {
    let mut total = 0i64;
    for p in ports {
        let words = read_holding("127.0.0.1", p, 1, 50, 2).await?;
        total += i64::from(decode_int32(&words, WordOrder::HighLow));
    }
    Ok(total)
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
async fn battery_step_seen_through_a_lagging_meter_does_not_double_the_target() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    // Arrange — [0, 0] envelope: POI may neither import nor export, so the
    // envelope's floor forces the battery to carry the whole 1.12 MW load.
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
            MODULE_ID: { "set_active_power": {
                "verb": "set", "target": "active_power", "unit": "watts",
                "protocol": "distribute", "allocation_policy": "equal_split",
                "ramp_rate_per_sec": 0.10, "hysteresis_margin": 0.05, "hysteresis_dwell_secs": 30.0,
                "power_min": -8_000_000.0, "power_max": 8_000_000.0,
                "import_limit_topic": "sites/{site_id}/devices/operating_envelope/measurements/import_limit/watts",
                "export_limit_topic": "sites/{site_id}/devices/operating_envelope/measurements/export_limit/watts",
                "active_power_topic": format!("sites/{{site_id}}/devices/{MODULE_ID}/measurements/active_power/watts"),
                "poi_active_power_topic": format!("sites/{{site_id}}/devices/{METER_ID}/measurements/active_power/watts"),
                "children": [child(RACKS[0]), child(RACKS[1])],
            }},
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
    fixtures::readiness::wait_for_gateway_ready(&broker_url, SITE_ID, &[MODULE_ID]).await?;
    let op = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&broker_url)
            .client_id("poi-lag-op")
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
    publish(&op, METER_ID, "active_power/watts", LOAD_W).await?;
    publish(&op, MODULE_ID, "active_power/watts", 0.0).await?;
    // The envelope's floor drives the racks to carry the load.
    timeout(Duration::from_secs(15), async {
        while racks_total(ports).await? != LOAD_W as i64 {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        anyhow::Ok(())
    })
    .await??;

    // Act — the battery now reports its 1.12 MW, but the meter hasn't caught
    // up and still shows the full load imported: raw site load reads 2.24 MW.
    publish(&op, MODULE_ID, "active_power/watts", LOAD_W).await?;
    let started = Instant::now();
    let mut peak = 0i64;
    while started.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(250)).await;
        peak = peak.max(racks_total(ports).await?);
    }
    let elapsed_s = started.elapsed().as_secs_f64();

    // Assert — the meter never catches up here, so the estimate keeps
    // ramping toward the 2.24 MW raw reading, but no faster than 1%/s of the
    // 8 MW rating (80 kW/s). One extra second of slack covers tick phase.
    // Unfiltered, the racks jump straight to 2.24 MW on the first tick.
    #[allow(clippy::cast_possible_truncation)]
    let bound = LOAD_W as i64 + (RAMP_W_PER_S * (elapsed_s + 1.0)) as i64;
    assert!(
        peak <= bound,
        "racks peaked at {peak} W after {elapsed_s:.1}s (bound {bound} W): \
         the lagged step reached the load estimate faster than the ramp"
    );

    cancel.cancel();
    gateway.await??;
    Ok(())
}
