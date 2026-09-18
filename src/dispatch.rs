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
//! Contract per ems-hmi `dispatchEvents.ts` (locked): `done` means the
//! south-side write succeeded — not that the device ramped or that the
//! setpoint took physical effect. The command's binding is resolved from
//! `x-command-source` (each entry carries `verb`+`target` explicitly, since
//! the topic never carries the template's own command name); Modbus TCP
//! bindings get a real write, every other protocol gets an explicit
//! `failed` (unsupported, not a silent no-op). Unknown device, unknown
//! command, and write errors all become `failed` too, with `reason` saying
//! which.
//!
//! Frames without a `command_id` can't be correlated by the HMI, so they are
//! logged and dropped rather than acked.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::ProtocolBinding;
use crate::config::GatewayCredentials;
use crate::modbus::client as modbus;
use anyhow::{Context, Result};
use paho_mqtt::AsyncClient;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
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

/// Parsed identity of a commands/ topic: which device, and which command
/// (verb + target — matched against each `x-command-source` entry's own
/// `verb`/`target` fields, joined `{verb}_{target}` to key the lookup map).
pub struct CommandTopic<'t> {
    /// The device the command targets.
    pub device_id: &'t str,
    /// The command verb (e.g. `set`, `enable`).
    pub verb: &'t str,
    /// The command target within the device (e.g. `active_power`).
    pub target: &'t str,
}

/// Parse a commands/ topic into device id + verb + target, scoped to our site.
///
/// Topic shape (system_adr §9):
/// `sites/{site}/devices/{dev}/commands/{verb}/{target}/{unit}`.
pub fn parse_command_topic<'t>(topic: &'t str, site_id: &str) -> Option<CommandTopic<'t>> {
    let mut parts = topic.split('/');
    if parts.next() != Some("sites")
        || parts.next() != Some(site_id)
        || parts.next() != Some("devices")
    {
        return None;
    }
    let device_id = parts.next().filter(|s| !s.is_empty())?;
    if parts.next() != Some("commands") {
        return None;
    }
    let verb = parts.next().filter(|s| !s.is_empty())?;
    let target = parts.next().filter(|s| !s.is_empty())?;
    Some(CommandTopic {
        device_id,
        verb,
        target,
    })
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
/// resolve binding → protocol write → `done`/`failed`. Unknown-site or
/// unparseable frames are logged and dropped (nothing to correlate an ack
/// to). `Phase::Done` means the write to the south-side device succeeded;
/// `Phase::Failed` covers unknown device/command, unsupported protocol, and
/// write errors alike.
#[allow(clippy::too_many_arguments)]
pub async fn handle_command(
    client: &AsyncClient,
    site_id: &str,
    device_channels: &HashMap<String, HashMap<String, ProtocolBinding>>,
    device_trust: &HashMap<String, DeviceTrust>,
    creds: Option<&GatewayCredentials>,
    topic: &str,
    payload: &[u8],
) -> Result<()> {
    let Some(cmd_topic) = parse_command_topic(topic, site_id) else {
        warn!(%topic, "command on unexpected topic; dropping");
        return Ok(());
    };
    let device_id = cmd_topic.device_id;
    let frame: CommandFrame = match serde_json::from_slice(payload) {
        Ok(f) => f,
        Err(err) => {
            warn!(%topic, error = %err, "command frame unparseable; dropping (no command_id to ack)");
            return Ok(());
        }
    };
    let events = event_topic(site_id, device_id);
    publish_event(client, &events, &frame.command_id, Phase::Received, None).await?;

    let Some(channels) = device_channels.get(device_id) else {
        warn!(%device_id, command_id = %frame.command_id, "dispatch rejected — device not in spec");
        return publish_event(
            client,
            &events,
            &frame.command_id,
            Phase::Failed,
            Some(&format!("unknown device {device_id}")),
        )
        .await;
    };
    let channel_key = format!("{}_{}", cmd_topic.verb, cmd_topic.target);
    let Some(binding) = channels.get(&channel_key) else {
        warn!(%device_id, %channel_key, command_id = %frame.command_id, "dispatch rejected — unknown command");
        return publish_event(
            client,
            &events,
            &frame.command_id,
            Phase::Failed,
            Some(&format!(
                "unknown command {channel_key} for device {device_id}"
            )),
        )
        .await;
    };

    match binding {
        ProtocolBinding::ModbusTcp(b) => {
            let trust = device_trust.get(device_id);
            match modbus::write_setpoint(b, frame.value, trust, creds).await {
                Ok(()) => {
                    info!(%device_id, command_id = %frame.command_id, value = frame.value, "dispatch write succeeded");
                    publish_event(client, &events, &frame.command_id, Phase::Done, None).await
                }
                Err(err) => {
                    warn!(%device_id, command_id = %frame.command_id, error = %err, "dispatch write failed");
                    publish_event(
                        client,
                        &events,
                        &frame.command_id,
                        Phase::Failed,
                        Some(&format!("modbus write failed: {err}")),
                    )
                    .await
                }
            }
        }
        other => {
            let protocol = protocol_name(other);
            warn!(%device_id, command_id = %frame.command_id, protocol, "dispatch rejected — unsupported protocol");
            publish_event(
                client,
                &events,
                &frame.command_id,
                Phase::Failed,
                Some(&format!("unsupported protocol for commands: {protocol}")),
            )
            .await
        }
    }
}

/// Human-readable protocol name for an unsupported-binding rejection reason.
fn protocol_name(binding: &ProtocolBinding) -> &'static str {
    match binding {
        ProtocolBinding::ModbusTcp(_) => "modbus_tcp",
        ProtocolBinding::Snmp(_) => "snmp",
        ProtocolBinding::Redfish(_) => "redfish",
        ProtocolBinding::Dnp3Tcp(_) => "dnp3_tcp",
        ProtocolBinding::BacnetIp(_) => "bacnet_ip",
        ProtocolBinding::BacnetSc(_) => "bacnet_sc",
        ProtocolBinding::Synthetic(_) => "synthetic",
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
    fn parses_device_verb_target_from_command_topic() {
        // Arrange
        let topic = "sites/s1/devices/bess_module_01/commands/set/active_power/watts";
        // Act
        let parsed = parse_command_topic(topic, "s1").unwrap();
        // Assert
        assert_eq!(parsed.device_id, "bess_module_01");
        assert_eq!(parsed.verb, "set");
        assert_eq!(parsed.target, "active_power");
    }

    #[test]
    fn rejects_other_site_and_non_command_topics() {
        // Arrange + Act + Assert — wrong site
        assert!(parse_command_topic("sites/other/devices/d/commands/set/x/w", "s1").is_none());
        // measurements family is not a command
        assert!(parse_command_topic("sites/s1/devices/d/measurements/x/w", "s1").is_none());
        // truncated topic — missing device
        assert!(parse_command_topic("sites/s1/devices", "s1").is_none());
        // truncated topic — missing target
        assert!(parse_command_topic("sites/s1/devices/d/commands/set", "s1").is_none());
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
