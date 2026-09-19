//! One async task per envelope-guarded `distribute` command: ticks at 1 Hz
//! (matching `active_power`'s poll rate), runs `EnvelopeController`, and
//! writes any resulting setpoint change via `dispatch::execute_setpoint` —
//! bypassing `handle_command`'s last-requested-setpoint capture entirely,
//! so the envelope loop's own clamped writes can never look like a new
//! real operator request.

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

/// Everything one envelope task needs to run forever.
pub struct EnvelopeTaskConfig {
    /// The module device this task guards.
    pub device_id: String,
    /// `{verb}_{target}` key into `device_channels`/`last_requested`.
    pub channel_key: String,
    /// The full (already `{site_id}`-substituted) guard config.
    pub guard: EnvelopeGuardConfig,
    /// The module's own distribute binding — passed to `execute_setpoint`
    /// unchanged; the envelope loop only ever changes what *value* gets
    /// dispatched through it, never the binding itself.
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
        let mut controller = EnvelopeController::new(cfg.guard.control, 0.0);
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

/// One tick: read live cache state, advance the controller, and write any
/// resulting change. Holds (does nothing) until every required cache entry
/// has landed — same posture as a synthetic task's hold semantic.
#[allow(clippy::too_many_arguments)]
async fn tick_once(
    cfg: &EnvelopeTaskConfig,
    controller: &mut EnvelopeController,
    dt: Duration,
    site_id: &str,
    cache: &InputCache,
    last_requested: &LastRequestedSetpoints,
    device_channels: &Arc<RwLock<HashMap<String, HashMap<String, ProtocolBinding>>>>,
    device_trust: &Arc<RwLock<HashMap<String, DeviceTrust>>>,
    creds: Option<&GatewayCredentials>,
) {
    let active_power_topic = cfg.guard.active_power_topic.replace("{site_id}", site_id);
    let Some(active_power) = cache.get(&active_power_topic).map(|e| e.0) else {
        return; // hold — no active_power reading cached yet
    };
    let import_limit_topic = cfg.guard.import_limit_topic.replace("{site_id}", site_id);
    let export_limit_topic = cfg.guard.export_limit_topic.replace("{site_id}", site_id);
    let import_limit = cache.get(&import_limit_topic).map(|e| e.0);
    let export_limit = cache.get(&export_limit_topic).map(|e| e.0);

    let requested_setpoint = last_requested
        .read()
        .await
        .get(&cfg.device_id)
        .and_then(|m| m.get(&cfg.channel_key))
        .copied()
        .unwrap_or(0.0);

    let Some(new_setpoint) = controller.tick(EnvelopeTick {
        import_limit,
        export_limit,
        active_power,
        requested_setpoint,
        power_min: cfg.guard.power_min,
        power_max: cfg.guard.power_max,
        dt,
    }) else {
        return; // unchanged this tick
    };

    let channels = device_channels.read().await;
    let trust = device_trust.read().await;
    if let Err(err) = dispatch::execute_setpoint(
        &cfg.binding,
        new_setpoint,
        &cfg.device_id,
        &cfg.channel_key,
        site_id,
        &channels,
        &trust,
        creds,
        cache,
    )
    .await
    {
        warn!(
            device_id = %cfg.device_id,
            value = new_setpoint,
            error = %err,
            "envelope actuation write failed",
        );
    }
}
