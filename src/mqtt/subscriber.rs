//! MQTT subscriber: receives `system/topology_changed` beacons.
//! Tier 1: logs only; reconcile is a future enhancement.

use anyhow::{Context, Result};
use futures::stream::StreamExt;
use paho_mqtt::AsyncClient;
use tracing::info;

/// MQTT topic the gateway subscribes to for topology-change beacons.
const TOPIC_TOPOLOGY_CHANGED: &str = "system/topology_changed";

/// Subscribe to `system/topology_changed` and spawn a logging task.
pub async fn subscribe_topology_changed(client: &mut AsyncClient) -> Result<()> {
    let mut stream = client.get_stream(64);
    client
        .subscribe(TOPIC_TOPOLOGY_CHANGED, 1)
        .await
        .context("subscribe to system/topology_changed")?;

    tokio::spawn(async move {
        while let Some(msg_opt) = stream.next().await {
            if let Some(msg) = msg_opt {
                info!(
                    topic = %msg.topic(),
                    payload = %String::from_utf8_lossy(msg.payload()),
                    "topology changed beacon",
                );
            }
        }
    });
    Ok(())
}
