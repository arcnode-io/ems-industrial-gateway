//! Dispatch command consumer — the gateway half of the dispatch contract.
//!
//! The HMI (operator role) publishes a command frame to
//! `sites/{site}/devices/{dev}/commands/{verb}/{target}/{unit}` and drives its
//! lifecycle UI from the gateway's acks on
//! `sites/{site}/devices/{dev}/events/dispatch_state`:
//!
//! ```json
//! { "ts": "...", "command_id": "...", "phase": "received|done|failed", "reason": "..." }
//! ```
//!
//! Contract per ems-hmi `dispatchEvents.ts` (locked): `done` means the gateway
//! ACCEPTED the setpoint — not that the device ramped. v1 acceptance =
//! device exists in the current AsyncAPI spec; the DTM schema carries no
//! writable command bindings yet, so there is no south-side write to perform.
//! When command bindings land in the template schema, the write happens
//! between `received` and `done` and a rejected write becomes `failed`.
//!
//! Frames without a `command_id` can't be correlated by the HMI, so they are
//! logged and dropped rather than acked.

use anyhow::{Context, Result};
use paho_mqtt::AsyncClient;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;
use tracing::{info, warn};

/// QoS for dispatch lifecycle events — at-least-once, same as the commands
/// family they answer (ADR-002 §11).
const EVENT_QOS: i32 = 1;

/// Inbound command frame published by the HMI on a commands/ topic.
#[derive(Debug, Deserialize)]
pub struct CommandFrame {
    /// Publisher wall-clock timestamp (RFC3339). Carried through unused.
    pub ts: String,
    /// Commanded value in the topic's engineering unit (e.g. watts).
    pub value: f64,
    /// Correlation id — the HMI matches acks to its in-flight command.
    pub command_id: String,
}

/// Lifecycle phase per the locked HMI contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Frame parsed + correlated — work begins.
    Received,
    /// Setpoint accepted (not "ramped"; see module docs).
    Done,
    /// Rejected — `reason` says why.
    Failed,
}

/// Parse a commands/ topic into its device id, scoped to our site.
///
/// Topic shape (7 segments, system_adr §9):
/// `sites/{site}/devices/{dev}/commands/{verb}/{target}/{unit}` — actually 8
/// path segments including the unit terminal; we only require the prefix
/// through `commands` and a non-empty device segment.
pub fn parse_command_topic<'t>(topic: &'t str, site_id: &str) -> Option<&'t str> {
    let mut parts = topic.split('/');
    (parts.next() == Some("sites")
        && parts.next() == Some(site_id)
        && parts.next() == Some("devices"))
    .then(|| parts.next())
    .flatten()
    .filter(|device| !device.is_empty() && parts.next() == Some("commands"))
}

/// The events topic a device's dispatch acks ride on (mirrors the HMI's
/// `dispatchStateTopic` in topicBuilder.ts — change only in lockstep).
pub fn event_topic(site_id: &str, device_id: &str) -> String {
    format!("sites/{site_id}/devices/{device_id}/events/dispatch_state")
}

/// Serialize one lifecycle event payload.
pub fn event_payload(
    now_rfc3339: &str,
    command_id: &str,
    phase: Phase,
    reason: Option<&str>,
) -> String {
    let mut v = json!({ "ts": now_rfc3339, "command_id": command_id, "phase": phase });
    if let Some(r) = reason {
        v["reason"] = json!(r);
    }
    v.to_string()
}

/// Handle one inbound commands/ message end-to-end: parse → `received` →
/// accept/reject → `done`/`failed`. Unknown-site or unparseable frames are
/// logged and dropped (nothing to correlate an ack to).
pub async fn handle_command(
    client: &AsyncClient,
    site_id: &str,
    known_devices: &BTreeSet<String>,
    topic: &str,
    payload: &[u8],
) -> Result<()> {
    let Some(device_id) = parse_command_topic(topic, site_id) else {
        warn!(%topic, "command on unexpected topic; dropping");
        return Ok(());
    };
    let frame: CommandFrame = match serde_json::from_slice(payload) {
        Ok(f) => f,
        Err(err) => {
            warn!(%topic, error = %err, "command frame unparseable; dropping (no command_id to ack)");
            return Ok(());
        }
    };
    let events = event_topic(site_id, device_id);
    publish_event(client, &events, &frame.command_id, Phase::Received, None).await?;
    if known_devices.contains(device_id) {
        info!(%device_id, command_id = %frame.command_id, value = frame.value, "dispatch accepted");
        publish_event(client, &events, &frame.command_id, Phase::Done, None).await
    } else {
        warn!(%device_id, command_id = %frame.command_id, "dispatch rejected — device not in spec");
        publish_event(
            client,
            &events,
            &frame.command_id,
            Phase::Failed,
            Some(&format!("unknown device {device_id}")),
        )
        .await
    }
}

/// Publish one lifecycle event at QoS 1, RETAINED. dispatch_state is a state
/// topic: the HMI subscribes when the operator confirms — milliseconds AFTER
/// the command publish — and the gateway's acks beat its SUBACK. Retained
/// delivery hands the late subscriber the latest state immediately (and a
/// mid-dispatch page refresh recovers it); the HMI's command_id correlation
/// discards stale events from prior commands.
async fn publish_event(
    client: &AsyncClient,
    topic: &str,
    command_id: &str,
    phase: Phase,
    reason: Option<&str>,
) -> Result<()> {
    let payload = event_payload(&chrono::Utc::now().to_rfc3339(), command_id, phase, reason);
    let msg = paho_mqtt::Message::new_retained(topic, payload, EVENT_QOS);
    client
        .publish(msg)
        .await
        .with_context(|| format!("publish dispatch event to {topic}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_from_command_topic() {
        // Arrange
        let topic = "sites/s1/devices/bess_module_01/commands/set/active_power/watts";
        // Act + Assert
        assert_eq!(parse_command_topic(topic, "s1"), Some("bess_module_01"));
    }

    #[test]
    fn rejects_other_site_and_non_command_topics() {
        // Arrange + Act + Assert — wrong site
        assert_eq!(
            parse_command_topic("sites/other/devices/d/commands/set/x/w", "s1"),
            None
        );
        // measurements family is not a command
        assert_eq!(
            parse_command_topic("sites/s1/devices/d/measurements/x/w", "s1"),
            None
        );
        // truncated topic
        assert_eq!(parse_command_topic("sites/s1/devices", "s1"), None);
    }

    #[test]
    fn event_payload_carries_contract_fields() {
        // Arrange + Act
        let done = event_payload("2026-07-03T00:00:00Z", "cmd-1", Phase::Done, None);
        let failed = event_payload(
            "2026-07-03T00:00:00Z",
            "cmd-2",
            Phase::Failed,
            Some("unknown device x"),
        );
        // Assert — exact wire contract per dispatchEvents.ts
        let d: serde_json::Value = serde_json::from_str(&done).unwrap();
        assert_eq!(d["phase"], "done");
        assert_eq!(d["command_id"], "cmd-1");
        assert!(d.get("reason").is_none());
        let f: serde_json::Value = serde_json::from_str(&failed).unwrap();
        assert_eq!(f["phase"], "failed");
        assert_eq!(f["reason"], "unknown device x");
    }

    #[test]
    fn command_frame_requires_command_id() {
        // Arrange — frame missing command_id (HMI can't correlate an ack)
        let bad = br#"{"ts":"t","value":1.0}"#;
        // Act + Assert
        assert!(serde_json::from_slice::<CommandFrame>(bad).is_err());
    }
}
