//! Site→module distribution: splits der_dispatch's `target_active_power`
//! across whatever `bess_module` devices exist, dispatched as real MQTT
//! commands — same pipeline as an operator's own command, so each module's
//! existing rebalance-to-racks machinery does its normal job underneath.
//! No new schema anywhere: eligibility comes from each module's own already-
//! resolved Distribute binding (power_min/power_max), weighting from its
//! own already-published state_of_charge.
//!
//! Hybrid trigger, same as module→rack: one tick both reacts to
//! target_active_power/event_active changing (poll cadence fast enough to
//! read as immediate) and rebalances on module SoC drift alone. Holds (no
//! publish) whenever event_active isn't true, or target/any module's own
//! state hasn't landed in cache yet, or nothing's actually changed since the
//! last dispatch — same "hold until known, write only on change" posture as
//! everywhere else in this codebase.

use crate::asyncapi::types::ProtocolBinding;
use crate::dispatch::allocation::{self, AllocationPolicy, ChildCapacity, OperatingState};
use crate::synthetic::InputCache;
use chrono::Utc;
use paho_mqtt::{AsyncClient, Message};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// QoS for the commands this task publishes — matches every other commands/
/// topic (ADR-002 §11, at-least-once).
const COMMAND_QOS: i32 = 1;
/// The verb+target every bess_module command shares — site distribution is
/// specifically about active power, same scope as Phase III overall.
const CHANNEL_KEY: &str = "set_active_power";

/// Everything the site-distribution task needs to run forever.
pub struct SiteDistributionConfig {
    /// Site slug, substituted into every module's command topic.
    pub site_id: String,
    /// `sites/{site}/devices/der_dispatch/measurements/target_active_power/watts`.
    pub target_topic: String,
    /// `sites/{site}/devices/der_dispatch/measurements/event_active/none`.
    pub event_active_topic: String,
    /// Tick cadence — matches the module rebalance task's (1 Hz).
    pub tick_hz: f64,
}

/// Spawn the site-distribution loop. Mirrors `envelope::task::spawn`'s
/// shutdown contract: the returned `JoinHandle` exits when `cancel` fires.
pub fn spawn(
    cfg: SiteDistributionConfig,
    cache: InputCache,
    mqtt: AsyncClient,
    device_channels: Arc<RwLock<HashMap<String, HashMap<String, ProtocolBinding>>>>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let period_ms = (1000.0 / cfg.tick_hz).max(1.0) as u64;
        let mut ticker = interval(Duration::from_millis(period_ms));
        // Last share actually dispatched per module — lets a tick where the
        // site target is unchanged still dispatch only modules whose own
        // SoC-weighted share drifted, and skip a redundant re-command
        // otherwise (a real command message, not a cheap in-process write).
        let mut last_dispatched: HashMap<String, f64> = HashMap::new();
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    tick_once(&cfg, &cache, &mqtt, &device_channels, &mut last_dispatched).await;
                }
            }
        }
    })
}

/// One tick: react to der_dispatch's live target/event_active, rebalance
/// across modules, dispatch only what changed.
async fn tick_once(
    cfg: &SiteDistributionConfig,
    cache: &InputCache,
    mqtt: &AsyncClient,
    device_channels: &Arc<RwLock<HashMap<String, HashMap<String, ProtocolBinding>>>>,
    last_dispatched: &mut HashMap<String, f64>,
) {
    let Some(event_active) = cache.get(&cfg.event_active_topic).map(|e| e.0) else {
        return; // hold — event_active not cached yet
    };
    if event_active < 0.5 {
        return; // hold — dispatch not currently active
    }
    let Some(target) = cache.get(&cfg.target_topic).map(|e| e.0) else {
        return; // hold — target not cached yet
    };

    let channels = device_channels.read().await;
    let Some(modules) = modules_with_bounds(&channels, cache, &cfg.site_id, target) else {
        return; // hold — no modules known, or any module's state not yet cached
    };
    drop(channels);
    if modules.is_empty() {
        return;
    }

    let shares = allocation::allocate(target, &modules, AllocationPolicy::SocWeighted);
    let changed: Vec<(String, f64)> = shares
        .into_iter()
        .filter(|(id, share)| {
            last_dispatched
                .get(id)
                .is_none_or(|prev| (share - prev).abs() >= f64::EPSILON)
        })
        .collect();

    for (module_id, share) in changed {
        if dispatch_one(mqtt, &cfg.site_id, &module_id, share).await {
            last_dispatched.insert(module_id, share);
        }
    }
}

/// Publish one real command message to `module_id`, exactly as an operator
/// would — `handle_command` takes it from there (acks, last_requested
/// capture, the module's own rebalance-to-racks machinery). Returns whether
/// the publish itself succeeded.
async fn dispatch_one(mqtt: &AsyncClient, site_id: &str, module_id: &str, share: f64) -> bool {
    let topic = format!("sites/{site_id}/devices/{module_id}/commands/set/active_power/watts");
    let command_id = format!(
        "site-dist-{}",
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let payload = format!(
        r#"{{"ts":"{ts}","value":{share},"command_id":"{command_id}"}}"#,
        ts = Utc::now().to_rfc3339(),
    );
    let msg = Message::new(topic.clone(), payload, COMMAND_QOS);
    match mqtt.publish(msg).await {
        Ok(()) => true,
        Err(err) => {
            warn!(%topic, error = %err, "site distribution command publish failed");
            false
        }
    }
}

/// Every distribute-parent device (a `bess_module`) with a resolvable
/// power_min/power_max (from its own Distribute binding) and cached
/// state_of_charge. `None` if any known module's state_of_charge isn't
/// cached yet — hold, same posture as everywhere else; an empty (but
/// `Some`) result means there are simply no modules yet.
fn modules_with_bounds(
    channels: &HashMap<String, HashMap<String, ProtocolBinding>>,
    cache: &InputCache,
    site_id: &str,
    target: f64,
) -> Option<Vec<ChildCapacity>> {
    let mut modules = Vec::new();
    for (device_id, commands) in channels {
        let Some(ProtocolBinding::Distribute(d)) = commands.get(CHANNEL_KEY) else {
            continue;
        };
        let (Some(power_min), Some(power_max)) = (d.power_min, d.power_max) else {
            continue;
        };
        let soc_topic =
            format!("sites/{site_id}/devices/{device_id}/measurements/state_of_charge/percent");
        let state_of_charge = cache.get(&soc_topic).map(|e| e.0)?;
        let headroom = if target < 0.0 {
            power_min.abs()
        } else {
            power_max
        };
        modules.push(ChildCapacity {
            device_id: device_id.clone(),
            operating_state: OperatingState::Standby,
            headroom,
            state_of_charge,
        });
    }
    Some(modules)
}

#[cfg(test)]
#[path = "site_distribution_test.rs"]
mod tests;
