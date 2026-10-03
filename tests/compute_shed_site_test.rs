//! e2e: compute shed across several compute modules on one POI. High-risk:
//! one controller per module each answering the whole site's import cuts it
//! once per module, driving the site into export. One site-wide controller
//! cuts every GPU by the same percentage of the site's total.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{start_hivemq, unique_network};
use fixtures::spec_stub::spawn_asyncapi_stub;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const POI: &str = "sites/local_site/devices/poi_meter/measurements/active_power/watts";
const IMPORT: &str = "sites/local_site/devices/operating_envelope/measurements/import_limit/watts";
const EXPORT: &str = "sites/local_site/devices/operating_envelope/measurements/export_limit/watts";

/// Two compute modules, one 1000 W GPU each, shed enabled on both.
fn spec(bmc_port: u16) -> Value {
    let gpu = json!({
        "verb": "set", "target": "gpu_1_power_limit", "unit": "watts",
        "protocol": "redfish", "host": "127.0.0.1", "port": bmc_port,
        "uri": "/Systems/HGX_Baseboard_0/Processors/GPU_SXM_1/EnvironmentMetrics",
        "json_pointer": "/PowerLimitWatts/SetPoint",
    });
    let module = |node: &str| {
        json!({ "set_power_limit": {
            "verb": "set", "target": "power_limit", "unit": "percent", "protocol": "power_cap",
            "children": [{ "device_id": node, "target": "gpu_1_power_limit", "min_w": 200.0, "max_w": 1000.0 }],
            "import_limit_topic": IMPORT.replace("local_site", "{site_id}"),
            "export_limit_topic": EXPORT.replace("local_site", "{site_id}"),
            "poi_active_power_topic": POI.replace("local_site", "{site_id}"),
            "hysteresis_margin": 0.05, "hysteresis_dwell_secs": 1.0, "ramp_rate_per_sec": 0.1,
        } })
    };
    json!({
        "info": { "version": "v1" },
        "x-protocol-source": {},
        "x-command-source": {
            "gpu_node_1": { "set_gpu_1_power_limit": gpu.clone() },
            "gpu_node_2": { "set_gpu_1_power_limit": gpu },
            "compute_module_1": module("gpu_node_1"),
            "compute_module_2": module("gpu_node_2"),
        },
    })
}

#[tokio::test]
async fn one_site_wide_cut_covers_the_import_once() -> Result<()> {
    // Arrange
    let network = unique_network();
    let hivemq = start_hivemq(&network).await?;
    let broker_url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let bmc = MockServer::start().await;
    Mock::given(method("PATCH"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&bmc)
        .await;
    let stub = spawn_asyncapi_stub(spec(bmc.address().port())).await;
    unsafe {
        std::env::set_var("MQTT_GATEWAY_PASSWORD", "test");
    }
    let cfg = Config {
        device_api_url: stub.uri(),
        broker_url: broker_url.clone(),
        mqtt_username: "arcnode_gateway".to_string(),
        site_id: "local_site".to_string(),
        log_level: "info".to_string(),
        gateway_credentials: None,
    };
    let cancel = CancellationToken::new();
    let gateway = {
        let cancel = cancel.clone();
        tokio::spawn(async move { app::run(cfg, cancel).await })
    };
    let feed = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&broker_url)
            .client_id("shed-site-feed")
            .finalize(),
    )?;
    feed.connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;

    // Act — 50 W over a zero envelope, held flat (storage has nothing left)
    let first_caps = timeout(Duration::from_secs(30), async {
        loop {
            for (topic, v) in [(IMPORT, 0.0), (EXPORT, 0.0), (POI, 50.0)] {
                let payload = json!({ "ts": "t", "value": v }).to_string();
                feed.publish(Message::new(topic, payload, 0)).await?;
            }
            let caps: Vec<f64> = bmc
                .received_requests()
                .await
                .unwrap_or_default()
                .iter()
                .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
                .filter_map(|b| {
                    b.pointer("/PowerLimitWatts/SetPoint")
                        .and_then(Value::as_f64)
                })
                .collect();
            if caps.len() >= 2 {
                return anyhow::Ok(caps);
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await??;

    // Assert — 50 W of a 2 kW fleet is 2.5% → 98% (whole percent): 980 W on
    // both GPUs. Per-module controllers would each cut 5%: 950 W.
    assert_eq!(&first_caps[..2], &[980.0, 980.0], "{first_caps:?}");

    cancel.cancel();
    gateway.await??;
    Ok(())
}
