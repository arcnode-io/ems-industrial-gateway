//! The gateway's current MQTT subscriptions, kept in step with the
//! topology: the beacon, the site's commands filter, and every input topic
//! the current spec needs. A topology change subscribes what was added and
//! unsubscribes what was removed; a broker reconnect re-issues whatever is
//! current.

use anyhow::{Context, Result};
use paho_mqtt::AsyncClient;
use std::sync::{Arc, Mutex};
use tracing::info;

/// MQTT topic the gateway subscribes to for topology-change beacons.
pub const TOPIC_TOPOLOGY_CHANGED: &str = "system/topology_changed";
/// Beacon device-api sends after a lockout is set or cleared.
pub const TOPIC_LOTO_CHANGED: &str = "system/loto_changed";
/// QoS for the beacon subscription. At-least-once is fine — `watch` collapses
/// duplicates into a single wake anyway.
const BEACON_QOS: i32 = 1;
/// QoS for measurement-channel subscriptions; matches ADR-002 §11 (measurements at QoS 0).
const MEASUREMENT_QOS: i32 = 0;
/// QoS for the commands/ subscription — at-least-once per ADR-002 §11.
const COMMAND_QOS: i32 = 1;

/// Every topic the gateway subscribes to, paired with its QoS: the topology
/// and lockout beacons, the site's commands filter, then each measurement input.
pub fn subscription_list(site_id: &str, input_topics: &[String]) -> (Vec<String>, Vec<i32>) {
    let mut topics = vec![
        TOPIC_TOPOLOGY_CHANGED.to_string(),
        TOPIC_LOTO_CHANGED.to_string(),
        format!("sites/{site_id}/devices/+/commands/#"),
    ];
    let mut qos = vec![BEACON_QOS, BEACON_QOS, COMMAND_QOS];
    for topic in input_topics {
        topics.push(topic.clone());
        qos.push(MEASUREMENT_QOS);
    }
    (topics, qos)
}

/// Topics in `new` but not `old` (to subscribe), and in `old` but not `new`
/// (to unsubscribe).
pub fn diff(old: &[String], new: &[String]) -> (Vec<String>, Vec<String>) {
    let added = new.iter().filter(|t| !old.contains(t)).cloned().collect();
    let removed = old.iter().filter(|t| !new.contains(t)).cloned().collect();
    (added, removed)
}

/// The current subscription list, shared with the reconnect callback.
#[derive(Debug, Clone)]
pub struct Subscriptions {
    /// Site whose commands filter is subscribed.
    site_id: String,
    /// Topics and their QoS, as last subscribed.
    current: Arc<Mutex<(Vec<String>, Vec<i32>)>>,
}

impl Subscriptions {
    /// The list for a spec's `input_topics` (not yet subscribed).
    #[must_use]
    pub fn new(site_id: &str, input_topics: &[String]) -> Self {
        Self {
            site_id: site_id.to_string(),
            current: Arc::new(Mutex::new(subscription_list(site_id, input_topics))),
        }
    }

    /// Topics and QoS as currently subscribed.
    #[must_use]
    pub fn current(&self) -> (Vec<String>, Vec<i32>) {
        self.current
            .lock()
            .expect("subscriptions lock poisoned")
            .clone()
    }

    /// Follow a topology change: subscribe the inputs `input_topics` adds,
    /// unsubscribe the ones it drops, and make it what reconnects re-issue.
    pub async fn update(&self, client: &AsyncClient, input_topics: &[String]) -> Result<()> {
        let (topics, qos) = subscription_list(&self.site_id, input_topics);
        let (added, removed) = diff(&self.current().0, &topics);
        if !removed.is_empty() {
            client
                .unsubscribe_many(&removed)
                .await
                .context("unsubscribe inputs the topology dropped")?;
        }
        if !added.is_empty() {
            // Only input topics come and go; the beacon and commands filter
            // are in every list.
            client
                .subscribe_many(&added, &vec![MEASUREMENT_QOS; added.len()])
                .await
                .context("subscribe inputs the topology added")?;
        }
        *self.current.lock().expect("subscriptions lock poisoned") = (topics, qos);
        info!(
            added = added.len(),
            removed = removed.len(),
            "subscriptions follow topology"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topics(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    #[test]
    fn subscription_list_covers_beacons_commands_and_every_input() {
        // Arrange
        let inputs = vec!["sites/s/devices/a/measurements/x/watts".to_string()];
        // Act
        let (topics, qos) = subscription_list("s", &inputs);
        // Assert — what reconnect re-issues must match the initial subscribe
        assert_eq!(
            topics,
            vec![
                TOPIC_TOPOLOGY_CHANGED.to_string(),
                TOPIC_LOTO_CHANGED.to_string(),
                "sites/s/devices/+/commands/#".to_string(),
                inputs[0].clone(),
            ]
        );
        assert_eq!(
            qos,
            vec![BEACON_QOS, BEACON_QOS, COMMAND_QOS, MEASUREMENT_QOS]
        );
    }

    #[test]
    fn a_topology_change_subscribes_new_inputs_and_drops_removed_ones() {
        // Arrange — b stays, a goes, c arrives
        let old = topics(&["a", "b"]);
        let new = topics(&["b", "c"]);
        // Act
        let (added, removed) = diff(&old, &new);
        // Assert
        assert_eq!(added, topics(&["c"]));
        assert_eq!(removed, topics(&["a"]));
    }

    #[test]
    fn an_unchanged_topology_touches_nothing() {
        let same = topics(&["a", "b"]);
        assert_eq!(diff(&same, &same), (vec![], vec![]));
    }
}
