//! e2e: storage that can't answer doesn't make compute wait. With every
//! rack below its reserve floor, import past the envelope sheds GPUs at
//! once instead of after the 30 s dwell meant for storage still ramping.
//! Contracts only: spec stub (device-api), broker topics, a stub BMC, mock
//! Modbus racks.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::{app, config::Config};
use fixtures::bmc::caps_written;
use fixtures::containers::{start_hivemq, start_mock_modbus_server_writable, unique_network};
use fixtures::spec_stub::spawn_asyncapi_stub;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const SITE_ID: &str = "local_site";
const MODULE_ID: &str = "bess_module_1";
const RACKS: [&str; 2] = ["rack_1", "rack_2"];
const GPU_URI: &str = "/Systems/HGX_Baseboard_0/Processors/GPU_SXM_1/EnvironmentMetrics";
const POI: &str = "sites/{site_id}/devices/poi_meter/measurements/active_power/watts";
const IMPORT_LIMIT: &str =
    "sites/{site_id}/devices/operating_envelope/measurements/import_limit/watts";
const EXPORT_LIMIT: &str =
    "sites/{site_id}/devices/operating_envelope/measurements/export_limit/watts";

fn rack(device_id: &str) -> Value {
    json!({
        "device_id": device_id,
        "operating_state_topic": format!("sites/{{site_id}}/devices/{device_id}/measurements/operating_state/none"),
        "state_of_charge_topic": format!("sites/{{site_id}}/devices/{device_id}/measurements/state_of_charge/percent"),
        "power_min": -500_000.0, "power_max": 500_000.0,
    })
}

fn rack_command(port: u16) -> Value {
    json!({
        "verb": "set", "target": "active_power", "unit": "watts",
        "protocol": "modbus_tcp", "host": "127.0.0.1", "port": port,
        "unit_id": "1", "address": 50, "scale": 1.0, "offset": 0.0,
    })
}

/// Compute module over two GPUs (shed enabled) and a battery module over
/// two racks with a 25% floor, both on the same POI and envelope.
fn spec(bmc_port: u16, rack_ports: [u16; 2]) -> Value {
    let gpu_limit = json!({
        "verb": "set", "target": "gpu_1_power_limit", "unit": "watts",
        "protocol": "redfish", "host": "127.0.0.1", "port": bmc_port,
        "uri": GPU_URI, "json_pointer": "/PowerLimitWatts/SetPoint",
    });
    let gpu = |d: &str| json!({ "device_id": d, "target": "gpu_1_power_limit", "min_w": 200.0, "max_w": 1000.0 });
    json!({
        "info": { "version": "v1" },
        "x-protocol-source": {},
        "x-command-source": {
            "gpu_node_01": { "set_gpu_1_power_limit": gpu_limit.clone() },
            "gpu_node_02": { "set_gpu_1_power_limit": gpu_limit },
            "compute_module_01": { "set_power_limit": {
                "verb": "set", "target": "power_limit", "unit": "percent",
                "protocol": "power_cap",
                "children": [gpu("gpu_node_01"), gpu("gpu_node_02")],
                "import_limit_topic": IMPORT_LIMIT, "export_limit_topic": EXPORT_LIMIT,
                "poi_active_power_topic": POI,
                "hysteresis_margin": 0.05, "hysteresis_dwell_secs": 30.0, "ramp_rate_per_sec": 0.1,
            } },
            MODULE_ID: { "set_active_power": {
                "verb": "set", "target": "active_power", "unit": "watts",
                "protocol": "distribute", "allocation_policy": "equal_split",
                "state_of_charge_floor_percent": 25.0,
                "ramp_rate_per_sec": 0.1, "hysteresis_margin": 0.05, "hysteresis_dwell_secs": 30.0,
                "power_min": -1_000_000.0, "power_max": 1_000_000.0,
                "import_limit_topic": IMPORT_LIMIT, "export_limit_topic": EXPORT_LIMIT,
                "active_power_topic": format!("sites/{{site_id}}/devices/{MODULE_ID}/measurements/active_power/watts"),
                "poi_active_power_topic": POI,
                "children": [rack(RACKS[0]), rack(RACKS[1])],
            } },
            RACKS[0]: { "set_active_power": rack_command(rack_ports[0]) },
            RACKS[1]: { "set_active_power": rack_command(rack_ports[1]) },
        },
    })
}

/// Publish `{device}/measurements/{measurement}` on the site.
async fn publish(feed: &AsyncClient, device: &str, measurement: &str, value: f64) -> Result<()> {
    let topic = format!("sites/{SITE_ID}/devices/{device}/measurements/{measurement}");
    let payload = json!({ "ts": "t", "value": value }).to_string();
    feed.publish(Message::new(topic, payload, 0)).await?;
    Ok(())
}

/// One second of site readings: envelope at zero import, the battery idle,
/// racks at `soc`, POI importing `poi_w`.
async fn second(feed: &AsyncClient, poi_w: f64, soc: f64) -> Result<()> {
    for rack in RACKS {
        publish(feed, rack, "operating_state/none", 0.0).await?;
        publish(feed, rack, "state_of_charge/percent", soc).await?;
    }
    publish(feed, MODULE_ID, "active_power/watts", 0.0).await?;
    publish(feed, "operating_envelope", "import_limit/watts", 0.0).await?;
    publish(feed, "operating_envelope", "export_limit/watts", 0.0).await?;
    publish(feed, "poi_meter", "active_power/watts", poi_w).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    Ok(())
}

#[tokio::test]
async fn compute_sheds_at_once_when_storage_is_at_its_floor() -> Result<()> {
    // Arrange
    let network = unique_network();
    let hivemq = start_hivemq(&network).await?;
    let broker_url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let (rack1, rack2) = (
        start_mock_modbus_server_writable().await?,
        start_mock_modbus_server_writable().await?,
    );
    let rack_ports = [
        rack1.get_host_port_ipv4(502).await?,
        rack2.get_host_port_ipv4(502).await?,
    ];
    let bmc = MockServer::start().await;
    Mock::given(method("PATCH"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&bmc)
        .await;
    let stub = spawn_asyncapi_stub(spec(bmc.address().port(), rack_ports)).await;
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
            .client_id("shed-storage-feed")
            .finalize(),
    )?;
    feed.connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;

    // Act — 300 W import past a zero envelope, racks below their 25% floor
    let shed = timeout(Duration::from_secs(10), async {
        while !caps_written(&bmc).await.iter().any(|&c| c < 1000.0) {
            second(&feed, 300.0, 20.0).await?;
        }
        anyhow::Ok(())
    })
    .await;

    // Assert — well inside the 30 s dwell
    assert!(shed.is_ok(), "compute waited on storage that can't answer");

    cancel.cancel();
    gateway.await??;
    Ok(())
}
