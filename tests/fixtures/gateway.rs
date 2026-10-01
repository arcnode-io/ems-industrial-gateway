//! Run a real gateway against a stub spec for a fixed time: for tests that
//! measure what the gateway does to devices (requests, connections).

use crate::fixtures::containers::{start_hivemq, unique_network};
use crate::fixtures::spec_stub::spawn_asyncapi_stub;
use anyhow::Result;
use ems_industrial_gateway::{app, config::Config};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Run a gateway against a one-device spec (`node_1` with `readings`) for
/// `secs` seconds, then stop it. Returns how long it took to stop.
pub async fn poll_for(
    site_id: &str,
    readings: serde_json::Map<String, Value>,
    secs: u64,
) -> Result<Duration> {
    let network = unique_network();
    let hivemq = start_hivemq(&network).await?;
    let broker_url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let stub = spawn_asyncapi_stub(json!({
        "info": { "version": "v1" }, "x-protocol-source": { "node_1": readings },
    }))
    .await;
    unsafe {
        std::env::set_var("MQTT_GATEWAY_PASSWORD", "test");
    }
    let cfg = Config {
        device_api_url: stub.uri(),
        broker_url,
        mqtt_username: "arcnode_gateway".to_string(),
        site_id: site_id.to_string(),
        log_level: "info".to_string(),
        gateway_credentials: None,
    };
    let cancel = CancellationToken::new();
    let gateway = {
        let cancel = cancel.clone();
        tokio::spawn(async move { app::run(cfg, cancel).await })
    };
    tokio::time::sleep(Duration::from_secs(secs)).await;
    let stopping = std::time::Instant::now();
    cancel.cancel();
    gateway.await??;
    Ok(stopping.elapsed())
}
