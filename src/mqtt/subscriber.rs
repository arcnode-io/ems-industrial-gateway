//! MQTT subscriber: handles both the `system/topology_changed` beacon AND
//! per-channel measurement topics that feed the synthetic-channel cache.
//!
//! One paho `get_stream()` per client (paho enforces this), so this module
//! owns the single subscriber stream and demuxes by topic: beacons increment
//! a `watch::Receiver<u64>` counter (collapsed to a single wake for the
//! reconciler); per-channel FloatSample messages write into the shared
//! `InputCache` for synthetic tasks to read on their next tick.

use crate::synthetic::InputCache;
use anyhow::{Context, Result};
use futures::stream::StreamExt;
use paho_mqtt::AsyncClient;
use serde::Deserialize;
use std::time::Instant;
use tokio::sync::watch;
use tracing::{info, trace, warn};

/// MQTT topic the gateway subscribes to for topology-change beacons.
const TOPIC_TOPOLOGY_CHANGED: &str = "system/topology_changed";
/// QoS for the beacon subscription. At-least-once is fine — `watch` collapses
/// duplicates into a single wake anyway.
const BEACON_QOS: i32 = 1;
/// QoS for measurement-channel subscriptions; matches ADR-002 §11 (measurements at QoS 0).
const MEASUREMENT_QOS: i32 = 0;
/// Size of the paho stream buffer. 1024 covers high-rate measurements + beacons.
const STREAM_CAPACITY: usize = 1024;

/// FloatSample wire shape — `{ts, value}` per ADR-002 §5. Only `value` is
/// pulled into the cache today; `ts` is ignored (cache stamps Instant::now()
/// for local-monotonic ordering, separate from the publisher's wall-clock ts).
#[derive(Debug, Deserialize)]
struct FloatSample {
    /// The numeric reading parsed from the JSON payload.
    value: f64,
}

/// Subscribe to `system/topology_changed` AND the given measurement topics in
/// one shot. The single forwarder task demuxes by topic:
/// - beacon → bump a `watch::Receiver<u64>` counter
/// - measurement → write `(value, Instant::now())` into `cache`
///
/// Caller keeps the returned receiver to await topology changes; cache writes
/// are observed by synthetic tasks polling the cache on their own tick.
pub async fn subscribe(
    client: &mut AsyncClient,
    input_topics: &[String],
    cache: InputCache,
) -> Result<watch::Receiver<u64>> {
    let mut stream = client.get_stream(STREAM_CAPACITY);
    client
        .subscribe(TOPIC_TOPOLOGY_CHANGED, BEACON_QOS)
        .await
        .context("subscribe to system/topology_changed")?;
    for topic in input_topics {
        client
            .subscribe(topic, MEASUREMENT_QOS)
            .await
            .with_context(|| format!("subscribe to {topic}"))?;
    }
    info!(
        input_topics = input_topics.len(),
        "MQTT subscriptions established",
    );

    let (tx, rx) = watch::channel(0u64);
    tokio::spawn(async move {
        let mut beacon_count = 0u64;
        while let Some(msg_opt) = stream.next().await {
            let Some(msg) = msg_opt else { continue };
            if msg.topic() == TOPIC_TOPOLOGY_CHANGED {
                beacon_count = beacon_count.wrapping_add(1);
                info!(
                    topic = %msg.topic(),
                    payload = %String::from_utf8_lossy(msg.payload()),
                    count = beacon_count,
                    "topology changed beacon",
                );
                // Receiver dropped means main loop is shutting down — exit quietly.
                if tx.send(beacon_count).is_err() {
                    break;
                }
            } else {
                cache_float_sample(&cache, msg.topic(), msg.payload());
            }
        }
    });
    Ok(rx)
}

/// Parse a FloatSample payload + write `(value, Instant::now())` into the
/// cache. Malformed payloads logged and dropped — one bad sample shouldn't
/// stop the subscriber loop.
fn cache_float_sample(cache: &InputCache, topic: &str, payload: &[u8]) {
    match serde_json::from_slice::<FloatSample>(payload) {
        Ok(sample) => {
            cache.insert(topic.to_string(), (sample.value, Instant::now()));
            trace!(%topic, value = sample.value, "input cached");
        }
        Err(err) => warn!(%topic, error = %err, "FloatSample parse failed; dropping"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::new_input_cache;

    #[test]
    fn cache_float_sample_inserts_valid_payload() {
        // Arrange
        let cache = new_input_cache();
        let topic = "sites/x/devices/y/measurements/z/watts";
        let payload = br#"{"ts":"2026-05-17T00:00:00Z","value":42.5}"#;
        // Act
        cache_float_sample(&cache, topic, payload);
        // Assert
        let entry = cache.get(topic).expect("topic should be cached");
        assert!((entry.0 - 42.5).abs() < f64::EPSILON);
    }

    #[test]
    fn cache_float_sample_drops_malformed_payload() {
        // Arrange — payload missing `value` field
        let cache = new_input_cache();
        cache_float_sample(&cache, "topic", b"not json");
        cache_float_sample(&cache, "topic", br#"{"ts":"now"}"#);
        // Assert — neither call inserted
        assert_eq!(cache.len(), 0);
    }
}
