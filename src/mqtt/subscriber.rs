//! MQTT subscriber: receives `system/topology_changed` beacons and exposes
//! them as a `watch::Receiver<u64>` (monotonic beacon counter) for the main
//! loop. `watch` coalesces rapid back-to-back beacons into a single wake.

use anyhow::{Context, Result};
use futures::stream::StreamExt;
use paho_mqtt::AsyncClient;
use tokio::sync::watch;
use tracing::info;

/// MQTT topic the gateway subscribes to for topology-change beacons.
const TOPIC_TOPOLOGY_CHANGED: &str = "system/topology_changed";
/// QoS for the beacon subscription. At-least-once is fine — `watch` collapses
/// duplicates into a single wake anyway.
const BEACON_QOS: i32 = 1;
/// Size of the paho stream buffer. Beacons are rare; 64 is generous.
const STREAM_CAPACITY: usize = 64;

/// Subscribe to `system/topology_changed` and spawn a forwarder that bumps a
/// `watch::Receiver<u64>` counter on every beacon. The receiver coalesces N
/// rapid beacons into a single wake — main loop sees "something changed,
/// re-fetch" without having to drain a queue.
pub async fn subscribe_topology_changed(client: &mut AsyncClient) -> Result<watch::Receiver<u64>> {
    let mut stream = client.get_stream(STREAM_CAPACITY);
    client
        .subscribe(TOPIC_TOPOLOGY_CHANGED, BEACON_QOS)
        .await
        .context("subscribe to system/topology_changed")?;

    let (tx, rx) = watch::channel(0u64);
    tokio::spawn(async move {
        let mut count = 0u64;
        while let Some(msg_opt) = stream.next().await {
            if let Some(msg) = msg_opt {
                count = count.wrapping_add(1);
                info!(
                    topic = %msg.topic(),
                    payload = %String::from_utf8_lossy(msg.payload()),
                    count,
                    "topology changed beacon",
                );
                // Receiver dropped means main loop is shutting down — exit quietly.
                if tx.send(count).is_err() {
                    break;
                }
            }
        }
    });
    Ok(rx)
}
