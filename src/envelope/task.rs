//! One async task per `distribute` command (envelope-guarded or plain):
//! ticks at 1 Hz (matching `active_power`'s poll rate), always recomputes
//! the SoC-weighted child split from the current requested setpoint (a
//! child's SoC drifting is enough to rebalance the split even when the
//! module-level target hasn't changed), applies the envelope control law's
//! clamp on top when guarded, and writes only the children whose share
//! actually changed since the last tick — bypassing `handle_command`'s
//! last-requested-setpoint capture entirely, so this task's own writes can
//! never look like a new real operator request.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::{DistributeBinding, ProtocolBinding};
use crate::config::GatewayCredentials;
use crate::dispatch::{self, LastRequestedSetpoints};
use crate::envelope::control_law::{EnvelopeConfig, EnvelopeController, EnvelopeTick};
use crate::synthetic::InputCache;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Resolved envelope-guard config for one `distribute` binding — `Some`
/// only when every guard field is present; a plain (non-guarded) distribute
/// binding has none of them. Topic fields are the raw (still-templated)
/// wire values; the caller substitutes `{site_id}` when building
/// `EnvelopeTaskConfig`, same as `app.rs` already does for synthetic inputs.
pub struct EnvelopeGuardConfig {
    /// `EnvelopeController`'s ramp/hysteresis parameters.
    pub control: EnvelopeConfig,
    /// The module's own static lower bound (`active_power.bounds.min`).
    pub power_min: f64,
    /// The module's own static upper bound (`active_power.bounds.max`).
    pub power_max: f64,
    /// MQTT topic template carrying the live `import_limit`.
    pub import_limit_topic: String,
    /// MQTT topic template carrying the live `export_limit`.
    pub export_limit_topic: String,
    /// MQTT topic template carrying the module's own live `active_power`.
    pub active_power_topic: String,
}

/// Extract envelope-guard config from a `distribute` binding. `None` if any
/// guard field is missing — the binding is a plain, unguarded distribute.
pub fn envelope_guard_config(d: &DistributeBinding) -> Option<EnvelopeGuardConfig> {
    Some(EnvelopeGuardConfig {
        control: EnvelopeConfig {
            ramp_rate_per_sec: d.ramp_rate_per_sec?,
            hysteresis_margin: d.hysteresis_margin?,
            hysteresis_dwell: Duration::from_secs_f64(d.hysteresis_dwell_secs?),
        },
        power_min: d.power_min?,
        power_max: d.power_max?,
        import_limit_topic: d.import_limit_topic.clone()?,
        export_limit_topic: d.export_limit_topic.clone()?,
        active_power_topic: d.active_power_topic.clone()?,
    })
}

/// Everything one rebalance/envelope task needs to run forever.
pub struct EnvelopeTaskConfig {
    /// The device (module, or a future site-level virtual parent) this task
    /// distributes for.
    pub device_id: String,
    /// `{verb}_{target}` key into `device_channels`/`last_requested`.
    pub channel_key: String,
    /// `Some` for an envelope-guarded distribute (headroom clamp applies);
    /// `None` for a plain distribute — every tick still rebalances the
    /// child split, just with no clamp on top.
    pub guard: Option<EnvelopeGuardConfig>,
    /// The distribute binding this task rebalances. Must be
    /// `ProtocolBinding::Distribute` — the only kind of binding this task
    /// is ever spawned for.
    pub binding: ProtocolBinding,
    /// Tick cadence — matches the module's `active_power` poll rate (1 Hz).
    pub tick_hz: f64,
}

/// Spawn the per-module envelope loop. Mirrors `synthetic::task::spawn`'s
/// shutdown contract: the returned `JoinHandle` exits when `cancel` fires.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    cfg: EnvelopeTaskConfig,
    site_id: String,
    cache: InputCache,
    last_requested: LastRequestedSetpoints,
    device_channels: Arc<RwLock<HashMap<String, HashMap<String, ProtocolBinding>>>>,
    device_trust: Arc<RwLock<HashMap<String, DeviceTrust>>>,
    creds: Option<GatewayCredentials>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let period_ms = (1000.0 / cfg.tick_hz).max(1.0) as u64;
        let mut ticker = interval(Duration::from_millis(period_ms));
        // The initial requested_setpoint (before any real command has ever
        // arrived) is 0.0 — conservative: nothing to ramp toward yet, and a
        // real command populates `last_requested` before this could matter.
        let mut controller = cfg
            .guard
            .as_ref()
            .map(|g| EnvelopeController::new(g.control, 0.0));
        // Last share actually written per child — lets a tick where the
        // module-level target is unchanged still write only the children
        // whose own SoC-weighted share drifted, and skip the rest.
        let mut last_written: HashMap<String, f64> = HashMap::new();
        let mut last_tick = Instant::now();
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    let now = Instant::now();
                    let dt = now.duration_since(last_tick);
                    last_tick = now;
                    tick_once(
                        &cfg,
                        &mut controller,
                        &mut last_written,
                        dt,
                        &site_id,
                        &cache,
                        &last_requested,
                        &device_channels,
                        &device_trust,
                        creds.as_ref(),
                    )
                    .await;
                }
            }
        }
    })
}

/// One tick: resolve this tick's module-level target (clamped, for a
/// guarded task; the raw requested setpoint otherwise), recompute the
/// SoC-weighted child split fresh from live cache state, and write only the
/// children whose share actually changed. Holds (does nothing) until every
/// required cache entry has landed — same posture as a synthetic task's
/// hold semantic.
#[allow(clippy::too_many_arguments)]
async fn tick_once(
    cfg: &EnvelopeTaskConfig,
    controller: &mut Option<EnvelopeController>,
    last_written: &mut HashMap<String, f64>,
    dt: Duration,
    site_id: &str,
    cache: &InputCache,
    last_requested: &LastRequestedSetpoints,
    device_channels: &Arc<RwLock<HashMap<String, HashMap<String, ProtocolBinding>>>>,
    device_trust: &Arc<RwLock<HashMap<String, DeviceTrust>>>,
    creds: Option<&GatewayCredentials>,
) {
    let requested_setpoint = last_requested
        .read()
        .await
        .get(&cfg.device_id)
        .and_then(|m| m.get(&cfg.channel_key))
        .copied()
        .unwrap_or(0.0);

    let target = match (&cfg.guard, controller.as_mut()) {
        (Some(guard), Some(ctrl)) => {
            let active_power_topic = guard.active_power_topic.replace("{site_id}", site_id);
            let Some(active_power) = cache.get(&active_power_topic).map(|e| e.0) else {
                return; // hold — no active_power reading cached yet
            };
            let import_limit_topic = guard.import_limit_topic.replace("{site_id}", site_id);
            let export_limit_topic = guard.export_limit_topic.replace("{site_id}", site_id);
            let import_limit = cache.get(&import_limit_topic).map(|e| e.0);
            let export_limit = cache.get(&export_limit_topic).map(|e| e.0);
            // The clamped/ramped value either way — `tick`'s Some/None only
            // says whether it *changed* this tick, but the rebalance below
            // needs the current target regardless.
            ctrl.tick(EnvelopeTick {
                import_limit,
                export_limit,
                active_power,
                requested_setpoint,
                power_min: guard.power_min,
                power_max: guard.power_max,
                dt,
            });
            ctrl.current_output()
        }
        _ => requested_setpoint,
    };

    let ProtocolBinding::Distribute(d) = &cfg.binding else {
        warn!(device_id = %cfg.device_id, "rebalance task's binding is not distribute; nothing to do");
        return;
    };
    let shares = match dispatch::compute_shares(d, target, site_id, cache) {
        Ok(s) => s,
        Err(_) => return, // hold — same posture as missing cache inputs above
    };
    let changed: Vec<(String, f64)> = shares
        .into_iter()
        .filter(|(id, share)| {
            last_written
                .get(id)
                .is_none_or(|prev| (share - prev).abs() >= f64::EPSILON)
        })
        .collect();
    if changed.is_empty() {
        return;
    }

    let channels = device_channels.read().await;
    let trust = device_trust.read().await;
    match dispatch::write_shares(&changed, &cfg.channel_key, &channels, &trust, creds).await {
        Ok(()) => {
            for (id, share) in changed {
                last_written.insert(id, share);
            }
        }
        Err(err) => {
            warn!(
                device_id = %cfg.device_id,
                target,
                error = %err,
                "rebalance write failed",
            );
        }
    }
}
