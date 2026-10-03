//! Site→module distribution: splits der_dispatch's `target_active_power`
//! across whatever `bess_module` devices exist, each share handed to the
//! same command handler an operator's command goes through, so each module's
//! existing rebalance-to-racks machinery does its normal job underneath.
//! In-process, not via the broker: commands are the operator's to publish,
//! and the gateway's broker identity can't.
//! No new schema anywhere: eligibility comes from each module's own already-
//! resolved Distribute binding (power_min/power_max), weighting from its
//! own already-published state_of_charge.
//!
//! Hybrid trigger, same as module→rack: one tick both reacts to
//! target_active_power/event_active changing (poll cadence fast enough to
//! read as immediate) and rebalances on module SoC drift alone. Holds (no
//! dispatch) while event_active, the target or any module's own state hasn't
//! landed in cache yet, or nothing's changed since the last dispatch.
//!
//! An event that ENDS is not a hold. The modules keep whatever setpoint was
//! last written, so holding would keep discharging into the reserve after
//! the utility let go. On event_active false, each module the event
//! dispatched is commanded back to its pre-event operator setpoint (0 if
//! none), unless an operator commanded it mid-event. See `event_memory`.

use crate::config::GatewayCredentials;
use crate::der_dispatch::SharedEventMemory;
use crate::der_dispatch::module_bounds::modules_with_bounds;
use crate::der_dispatch::module_command::{Devices, dispatch_one};
use crate::dispatch::LastRequestedSetpoints;
use crate::dispatch::allocation::{self, AllocationPolicy};
use crate::synthetic::{InputCache, as_number};
use paho_mqtt::AsyncClient;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;

/// The verb+target every bess_module command shares — site distribution is
/// specifically about active power, same scope as Phase III overall.
pub(super) const CHANNEL_KEY: &str = "set_active_power";

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
    /// mTLS material for the south-side writes each command ends in.
    pub creds: Option<GatewayCredentials>,
}

/// Spawn the site-distribution loop. Mirrors `envelope::task::spawn`'s
/// shutdown contract: the returned `JoinHandle` exits when `cancel` fires.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    cfg: SiteDistributionConfig,
    cache: InputCache,
    mqtt: AsyncClient,
    devices: Devices,
    last_requested: LastRequestedSetpoints,
    memory: SharedEventMemory,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let period_ms = (1000.0 / cfg.tick_hz).max(1.0) as u64;
        let mut ticker = interval(Duration::from_millis(period_ms));
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    tick_once(&cfg, &cache, &mqtt, &devices, &last_requested, &memory).await;
                }
            }
        }
    })
}

/// Each module's current operator setpoint, as `last_requested` records it.
async fn operator_setpoints(last_requested: &LastRequestedSetpoints) -> HashMap<String, f64> {
    last_requested
        .read()
        .await
        .iter()
        .filter_map(|(device, channels)| {
            channels.get(CHANNEL_KEY).map(|r| (device.clone(), r.value))
        })
        .collect()
}

/// Event over: command each module the event dispatched back to its
/// pre-event setpoint. Memory is cleared only once every restore publishes,
/// so a failed publish is retried next tick rather than lost.
#[allow(clippy::too_many_arguments)]
async fn release(
    cfg: &SiteDistributionConfig,
    cache: &InputCache,
    mqtt: &AsyncClient,
    devices: &Devices,
    last_requested: &LastRequestedSetpoints,
    memory: &SharedEventMemory,
) {
    let current = operator_setpoints(last_requested).await;
    let restore = memory.lock().unwrap().end(&current);
    let mut all_published = true;
    for (module_id, setpoint) in restore {
        all_published &= dispatch_one(
            cfg,
            cache,
            mqtt,
            devices,
            last_requested,
            &module_id,
            setpoint,
        )
        .await;
    }
    if all_published {
        memory.lock().unwrap().clear();
    }
}

/// One tick: react to der_dispatch's live target/event_active, rebalance
/// across modules, dispatch only what changed, or release an ended event.
#[allow(clippy::too_many_arguments)]
async fn tick_once(
    cfg: &SiteDistributionConfig,
    cache: &InputCache,
    mqtt: &AsyncClient,
    devices: &Devices,
    last_requested: &LastRequestedSetpoints,
    memory: &SharedEventMemory,
) {
    let Some(event_active) = cache
        .get(&cfg.event_active_topic)
        .and_then(|e| as_number(&e.0))
    else {
        return; // hold — event_active not cached yet
    };
    if event_active < 0.5 {
        release(cfg, cache, mqtt, devices, last_requested, memory).await;
        return;
    }
    let operator = operator_setpoints(last_requested).await;
    memory.lock().unwrap().begin(|| operator);
    let Some(target) = cache.get(&cfg.target_topic).and_then(|e| as_number(&e.0)) else {
        return; // hold — target not cached yet
    };

    let channels = devices.channels.read().await;
    let Some(modules) = modules_with_bounds(&channels, cache, &cfg.site_id, target) else {
        return; // hold — no modules known, or any module's state not yet cached
    };
    drop(channels);
    if modules.is_empty() {
        return;
    }

    let shares = allocation::allocate(target, &modules, AllocationPolicy::SocWeighted);
    let changed = memory.lock().unwrap().changed(shares);
    for (module_id, share) in changed {
        if dispatch_one(cfg, cache, mqtt, devices, last_requested, &module_id, share).await {
            memory.lock().unwrap().record(&module_id, share);
        }
    }
}
