//! e2e: a lagging POI meter must not make the envelope ring. The meter's
//! reading trails the battery's by 1–3 s; a law that feeds on its own lag
//! rings into export. Closed loop here: the test plays
//! the meter, reading the racks and reporting the POI 2 s late.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::modbus::client::{WordOrder, decode_int32, read_holding};
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{start_hivemq, start_mock_modbus_server_writable};
use fixtures::spec_stub::spawn_asyncapi_stub;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const SITE_ID: &str = "site_001";
const MODULE_ID: &str = "bess_module_1";
const METER_ID: &str = "meter_01";
const RACKS: [&str; 2] = ["rack_1", "rack_2"];
const LOAD_W: f64 = 1_120_000.0;
/// How far the meter's reading trails the racks.
const METER_LAG: Duration = Duration::from_secs(2);
/// How long the loop runs; the approach settles in ~30 s.
const RUN_FOR: Duration = Duration::from_secs(45);

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
async fn a_lagging_meter_does_not_ring_the_envelope_into_export() -> Result<()> {
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
    // Seed the loop so the envelope has a POI reading to start from; the
    // rack registers only exist once the gateway's first write lands. A
    // failed probe read retries for up to ~15 s itself, hence the 30 s wait.
    publish(&op, MODULE_ID, "active_power/watts", 0.0).await?;
    publish(&op, METER_ID, "active_power/watts", LOAD_W).await?;
    timeout(Duration::from_secs(30), async {
        while racks_total(ports).await.is_err() {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await?;

    // Act — close the loop: module reading is fresh, meter reading is 2 s old
    // Reason: lag by wall time, not sample count; each rack read opens a
    // Modbus session, so the loop's own period drifts well past 500 ms.
    let mut meter: VecDeque<(Instant, f64)> = VecDeque::new();
    let mut reported = LOAD_W;
    let mut true_poi = Vec::new();
    let started = Instant::now();
    while started.elapsed() < RUN_FOR {
        #[allow(clippy::cast_precision_loss)]
        let battery = racks_total(ports).await? as f64;
        true_poi.push(LOAD_W - battery);
        meter.push_back((Instant::now(), LOAD_W - battery));
        while meter.front().is_some_and(|(t, _)| t.elapsed() >= METER_LAG) {
            reported = meter.pop_front().unwrap().1;
        }
        publish(&op, MODULE_ID, "active_power/watts", battery).await?;
        publish(&op, METER_ID, "active_power/watts", reported).await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Assert — no ringing into export, and the battery ends up carrying the
    // load. Reason for the 1% slack: with ~1 s sampling and a 1 s tick on top
    // of the 2 s meter lag, the loop sees ~3.5 s; the approach gain's design
    // bound there is ~1.3% overshoot. A ringing law exported ~50%.
    let worst = true_poi.iter().copied().fold(f64::INFINITY, f64::min);
    assert!(worst >= -0.01 * LOAD_W, "POI exported {:.0} W", -worst);
    let last = *true_poi.last().unwrap();
    assert!(
        last.abs() < 0.05 * LOAD_W,
        "POI settled at {last:.0} W, not ~0"
    );

    cancel.cancel();
    gateway.await??;
    Ok(())
}
