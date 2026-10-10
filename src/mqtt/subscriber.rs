//! MQTT subscriber: handles the `system/topology_changed` and
//! `system/loto_changed` beacons, commands, AND per-channel measurement
//! topics that feed the synthetic-channel cache.
//!
//! One paho `get_stream()` per client (paho enforces this), so this module
//! owns the single subscriber stream and demuxes by topic: beacons increment
//! a `watch::Receiver<u64>` counter (collapsed to a single wake for the
//! reconciler); per-channel samples write into the shared
//! `InputCache` for synthetic tasks to read on their next tick.

use crate::config::GatewayCredentials;
use crate::dispatch::{self, Devices, LastRequestedSetpoints};
use crate::mqtt::subscriptions::{Subscriptions, TOPIC_LOTO_CHANGED, TOPIC_TOPOLOGY_CHANGED};
use crate::synthetic::InputCache;
use anyhow::{Context, Result};
use futures::stream::StreamExt;
use paho_mqtt::AsyncClient;
use serde::Deserialize;
use std::time::Instant;
use tokio::sync::watch;
use tracing::{info, trace, warn};

/// Size of the paho stream buffer. 1024 covers high-rate measurements + beacons.
const STREAM_CAPACITY: usize = 1024;

/// A measurement sample, `{ts, value}`. Only `value` is cached; `ts` is
/// ignored (the cache stamps its own `Instant` for local ordering).
#[derive(Debug, Deserialize)]
struct Sample {
    /// The reading as published: a number, a boolean or an enum label.
    value: serde_json::Value,
}

/// Subscribe to `system/topology_changed` AND the given measurement topics in
/// one shot. The single forwarder task demuxes by topic:
/// - beacon → bump a `watch::Receiver<u64>` counter
/// - measurement → write `(value, Instant::now())` into `cache`
///
/// Caller keeps the returned receiver to await topology changes; cache writes
/// are observed by synthetic tasks polling the cache on their own tick.
#[allow(clippy::too_many_arguments)]
pub async fn subscribe(
    client: &mut AsyncClient,
    input_topics: &[String],
    cache: InputCache,
    site_id: &str,
    devices: Devices,
    loto_beacons: watch::Sender<u64>,
    creds: Option<GatewayCredentials>,
    last_requested: LastRequestedSetpoints,
) -> Result<(watch::Receiver<u64>, Subscriptions)> {
    let mut stream = client.get_stream(STREAM_CAPACITY);
    // Beacon, dispatch commands (HMI operator → gateway, acked on
    // events/dispatch_state by dispatch::handle_command), then every input.
    let subscriptions = Subscriptions::new(site_id, input_topics);
    let (topics, qos) = subscriptions.current();
    client
        .subscribe_many(&topics, &qos)
        .await
        .context("subscribe to beacon, commands and input topics")?;
    info!(
        input_topics = input_topics.len(),
        "MQTT subscriptions established",
    );
    // Reason: subscriptions are broker-side session state. After a broker
    // restart paho reconnects the socket but the broker holds none, so the
    // gateway would go silently deaf. Re-issue whatever is current (the
    // topology may have changed since boot) on every reconnect.
    let on_reconnect = subscriptions.clone();
    client.set_connected_callback(move |cli: &AsyncClient| {
        let (topics, qos) = on_reconnect.current();
        cli.subscribe_many(&topics, &qos);
        info!(topics = topics.len(), "MQTT reconnected; resubscribed");
    });

    let (tx, rx) = watch::channel(0u64);
    let event_client = client.clone();
    let site = site_id.to_string();
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
            } else if msg.topic() == TOPIC_LOTO_CHANGED {
                loto_beacons.send_modify(|n| *n = n.wrapping_add(1));
            } else if msg.topic().contains("/commands/") {
                let channels = devices.channels.read().await;
                let trust = devices.trust.read().await;
                let locked = devices.locked.read().await;
                if let Err(err) = dispatch::handle_command(
                    &event_client,
                    &site,
                    &channels,
                    &trust,
                    &locked,
                    creds.as_ref(),
                    &cache,
                    &last_requested,
                    msg.topic(),
                    msg.payload(),
                )
                .await
                {
                    warn!(topic = %msg.topic(), error = %err, "dispatch ack publish failed");
                }
            } else {
                cache_sample(&cache, msg.topic(), msg.payload());
            }
        }
    });
    Ok((rx, subscriptions))
}

/// Parse a sample payload + write `(value, Instant::now())` into the
/// cache. Malformed payloads logged and dropped — one bad sample shouldn't
/// stop the subscriber loop.
fn cache_sample(cache: &InputCache, topic: &str, payload: &[u8]) {
    match serde_json::from_slice::<Sample>(payload) {
        Ok(Sample { value }) if value.is_number() || value.is_boolean() || value.is_string() => {
            trace!(%topic, %value, "input cached");
            cache.insert(topic.to_string(), (value, Instant::now()));
        }
        Ok(Sample { value }) => {
            warn!(%topic, %value, "sample value is not a measurement; dropping")
        }
        Err(err) => warn!(%topic, error = %err, "sample parse failed; dropping"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::{as_number, new_input_cache};

    #[test]
    fn cache_sample_inserts_valid_payload() {
        // Arrange
        let cache = new_input_cache();
        let topic = "sites/x/devices/y/measurements/z/watts";
        let payload = br#"{"ts":"2026-05-17T00:00:00Z","value":42.5}"#;
        // Act
        cache_sample(&cache, topic, payload);
        // Assert
        let entry = cache.get(topic).expect("topic should be cached");
        assert_eq!(entry.0, serde_json::json!(42.5));
    }

    #[test]
    fn cache_sample_drops_malformed_payload() {
        // Arrange — payload missing `value` field
        let cache = new_input_cache();
        cache_sample(&cache, "topic", b"not json");
        cache_sample(&cache, "topic", br#"{"ts":"now"}"#);
        // Assert — neither call inserted
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn a_label_sample_is_cached_as_its_label() {
        // A rack's operating_state arrives as "FAULT"; distribution reads it
        let cache = new_input_cache();
        cache_sample(&cache, "s", br#"{"ts":"now","value":"FAULT"}"#);
        assert_eq!(cache.get("s").unwrap().0, serde_json::json!("FAULT"));
    }

    #[test]
    fn cache_sample_coerces_boolean_value_to_one_or_zero() {
        // Arrange — BooleanSample wire shape (der_dispatch.event_active etc.):
        // `{ts, value: true|false}`, a JSON bool, not a number.
        let cache = new_input_cache();
        cache_sample(&cache, "t", br#"{"ts":"now","value":true}"#);
        cache_sample(&cache, "f", br#"{"ts":"now","value":false}"#);
        // Assert
        assert_eq!(as_number(&cache.get("t").unwrap().0), Some(1.0));
        assert_eq!(as_number(&cache.get("f").unwrap().0), Some(0.0));
    }
}
