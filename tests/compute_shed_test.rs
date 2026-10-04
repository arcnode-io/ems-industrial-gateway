//! e2e: compute shed. A site importing past a zero envelope gets its GPUs
//! capped through each GPU's own Redfish limit, and uncapped again when the
//! envelope lifts. Contracts only: spec stub (device-api), broker topics,
//! a stub BMC.

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

const SITE_ID: &str = "local_site";
const GPU_URI: &str = "/Systems/HGX_Baseboard_0/Processors/GPU_SXM_1/EnvironmentMetrics";
const POI: &str = "sites/local_site/devices/poi_meter/measurements/active_power/watts";
const IMPORT_LIMIT: &str =
    "sites/local_site/devices/operating_envelope/measurements/import_limit/watts";
const EXPORT_LIMIT: &str =
    "sites/local_site/devices/operating_envelope/measurements/export_limit/watts";

/// compute_module capping two gpu_nodes' GPU 1, shed enabled (guard present).
fn spec(bmc_port: u16) -> Value {
    let gpu_limit = json!({
        "verb": "set", "target": "gpu_1_power_limit", "unit": "watts",
        "protocol": "redfish", "host": "127.0.0.1", "port": bmc_port,
        "uri": GPU_URI, "json_pointer": "/PowerLimitWatts/SetPoint",
    });
    let child = |d: &str| json!({ "device_id": d, "target": "gpu_1_power_limit", "min_w": 200.0, "max_w": 1000.0 });
    json!({
        "info": { "version": "v1" },
        "x-protocol-source": {},
        "x-command-source": {
            "gpu_node_01": { "set_gpu_1_power_limit": gpu_limit.clone() },
            "gpu_node_02": { "set_gpu_1_power_limit": gpu_limit },
            "compute_module_01": { "set_power_limit": {
                "verb": "set", "target": "power_limit", "unit": "percent",
                "protocol": "power_cap",
                "children": [child("gpu_node_01"), child("gpu_node_02")],
                "import_limit_topic": IMPORT_LIMIT.replace("local_site", "{site_id}"),
                "export_limit_topic": EXPORT_LIMIT.replace("local_site", "{site_id}"),
                "poi_active_power_topic": POI.replace("local_site", "{site_id}"),
                "hysteresis_margin": 0.05, "hysteresis_dwell_secs": 1.0, "ramp_rate_per_sec": 0.1,
            } },
        },
    })
}

/// Cap values the BMC has been sent so far.
async fn caps_written(bmc: &MockServer) -> Vec<f64> {
    bmc.received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
        .filter_map(|b| {
            b.pointer("/PowerLimitWatts/SetPoint")
                .and_then(Value::as_f64)
        })
        .collect()
}

/// Publish the envelope and a POI reading every 300 ms until `done` holds.
async fn drive_until(
    feed: &AsyncClient,
    bmc: &MockServer,
    poi_w: f64,
    import_limit: f64,
    done: impl Fn(&[f64]) -> bool,
) -> Result<Vec<f64>> {
    let found = timeout(Duration::from_secs(30), async {
        loop {
            for (topic, v) in [
                (IMPORT_LIMIT, import_limit),
                (EXPORT_LIMIT, 0.0),
                (POI, poi_w),
            ] {
                let payload = json!({ "ts": "t", "value": v }).to_string();
                feed.publish(Message::new(topic, payload, 0)).await?;
            }
            let caps = caps_written(bmc).await;
            if done(&caps) {
                return anyhow::Ok(caps);
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await??;
    Ok(found)
}

#[tokio::test]
async fn gpus_are_capped_while_import_breaks_the_envelope_then_restored() -> Result<()> {
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
        site_id: SITE_ID.to_string(),
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
            .client_id("shed-test-feed")
            .finalize(),
    )?;
    feed.connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;

    // Act — 300 W import against a zero envelope; storage isn't covering it
    let shed = drive_until(&feed, &bmc, 300.0, 0.0, |caps| {
        caps.iter().any(|&c| c < 1000.0)
    })
    .await?;
    // Assert — both GPUs capped below their max, never below their floor
    assert!(
        shed.iter().all(|&c| (200.0..=1000.0).contains(&c)),
        "{shed:?}"
    );

    // Act — the envelope lifts
    bmc.reset().await;
    Mock::given(method("PATCH"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&bmc)
        .await;
    let restored = drive_until(&feed, &bmc, 0.0, 1_000_000.0, |caps| {
        caps.iter().filter(|&&c| c == 1000.0).count() >= 2
    })
    .await?;
    // Assert — back to full power on both
    assert!(restored.contains(&1000.0), "{restored:?}");

    cancel.cancel();
    gateway.await??;
    Ok(())
}
