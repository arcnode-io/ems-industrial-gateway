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

pub mod allocation;
#[cfg(test)]
mod allocation_test;
mod distribute;
mod topic;

pub(crate) use distribute::{compute_shares, write_shares};
pub use topic::{CommandTopic, parse_command_topic};

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::ProtocolBinding;
use crate::config::GatewayCredentials;
use crate::modbus::client as modbus;
use crate::synthetic::InputCache;
use anyhow::{Context, Result, anyhow};
use paho_mqtt::AsyncClient;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Per-device, per-`{verb}_{target}` last real operator/dispatcher setpoint
/// request — captured in `handle_command` before any envelope clamp, so the
/// (future) envelope actuation loop always has a real value to ramp back
/// toward. The envelope loop's own writes go through `execute_setpoint`
/// directly, never through `handle_command`, so they can never overwrite
/// this — see `envelope` module docs.
pub type LastRequestedSetpoints = Arc<RwLock<HashMap<String, HashMap<String, f64>>>>;

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
    cache: &InputCache,
    last_requested: &RwLock<HashMap<String, HashMap<String, f64>>>,
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

    // Real operator/dispatcher request — capture before dispatching. See
    // `LastRequestedSetpoints` docs for why this must be the only writer.
    last_requested
        .write()
        .await
        .entry(device_id.to_string())
        .or_default()
        .insert(channel_key.clone(), frame.value);

    match execute_setpoint(
        binding,
        frame.value,
        device_id,
        &channel_key,
        site_id,
        device_channels,
        device_trust,
        creds,
        cache,
    )
    .await
    {
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
                Some(&format!("{err}")),
            )
            .await
        }
    }
}

/// Dispatch `value` through whatever `binding` actually is — Modbus TCP
/// writes directly, Distribute fans out via max-min fair allocation. The
/// one south-side write mechanism shared by real inbound commands
/// (`handle_command`, above) and the envelope actuation loop — whichever
/// calls this, the underlying write is identical.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_setpoint(
    binding: &ProtocolBinding,
    value: f64,
    device_id: &str,
    channel_key: &str,
    site_id: &str,
    device_channels: &HashMap<String, HashMap<String, ProtocolBinding>>,
    device_trust: &HashMap<String, DeviceTrust>,
    creds: Option<&GatewayCredentials>,
    cache: &InputCache,
) -> Result<()> {
    match binding {
        ProtocolBinding::ModbusTcp(b) => {
            let trust = device_trust.get(device_id);
            modbus::write_setpoint(b, value, trust, creds).await
        }
        ProtocolBinding::Distribute(d) => {
            distribute::dispatch_distribute(
                d,
                value,
                channel_key,
                site_id,
                device_channels,
                device_trust,
                creds,
                cache,
            )
            .await
        }
        other => Err(anyhow!(
            "unsupported protocol for commands: {}",
            protocol_name(other)
        )),
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
        ProtocolBinding::Distribute(_) => "distribute",
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
#[path = "mod_test.rs"]
mod tests;
