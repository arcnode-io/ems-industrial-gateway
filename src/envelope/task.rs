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
use crate::asyncapi::types::ProtocolBinding;
use crate::config::GatewayCredentials;
use crate::dispatch::{self, LastRequestedSetpoints};
use crate::envelope::config::EnvelopeTaskConfig;
use crate::envelope::control_law::{EnvelopeController, EnvelopeTick};
use crate::envelope::storage_authorized;
use crate::envelope::writes::WriteState;
use crate::synthetic::{InputCache, as_number};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::warn;

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
        // Built on the first tick with every input present, seeded with that
        // tick's active_power: the device's real starting output, not an
        // assumed 0. The initial requested_setpoint (before any real
        // command) is 0.0.
        let mut controller: Option<EnvelopeController> = None;
        let mut writes = WriteState::default();
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
                        &mut writes,
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
    writes: &mut WriteState,
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

    let target = match &cfg.guard {
        Some(guard) => {
            let active_power_topic = guard.active_power_topic.replace("{site_id}", site_id);
            let Some(active_power) = cache.get(&active_power_topic).and_then(|e| as_number(&e.0))
            else {
                return; // hold — no active_power reading cached yet
            };
            // With a POI meter configured but no reading yet, hold: falling
            // back to the battery-only law would ignore site load and let
            // charging push POI import past its limit.
            let poi_active_power = match &guard.poi_active_power_topic {
                Some(topic) => {
                    let topic = topic.replace("{site_id}", site_id);
                    let Some((p_poi, received_at)) = cache
                        .get(&topic)
                        .and_then(|e| Some((as_number(&e.0)?, e.1)))
                    else {
                        return; // hold — no POI reading cached yet
                    };
                    Some((p_poi, received_at))
                }
                None => None,
            };
            let import_limit_topic = guard.import_limit_topic.replace("{site_id}", site_id);
            let export_limit_topic = guard.export_limit_topic.replace("{site_id}", site_id);
            let import_limit = cache.get(&import_limit_topic).and_then(|e| as_number(&e.0));
            let export_limit = cache.get(&export_limit_topic).and_then(|e| as_number(&e.0));
            let (poi_fresh, hold_approach) = match poi_active_power {
                Some((_, received_at)) => writes.gate.take(received_at),
                None => (true, false),
            };
            // Built here, once every input is present, so its seed is this
            // tick's reading. Reason: building it on a tick that then held
            // seeded it from whatever was cached first, often a stale 0.
            let ctrl = controller
                .get_or_insert_with(|| EnvelopeController::new(guard.control, active_power));
            // The clamped/ramped value either way — `tick`'s Some/None only
            // says whether it *changed* this tick, but the rebalance below
            // needs the current target regardless.
            ctrl.tick(EnvelopeTick {
                import_limit,
                export_limit,
                active_power,
                requested_setpoint,
                poi_active_power: poi_active_power.map(|(p, _)| p),
                poi_fresh,
                hold_approach,
                power_min: guard.power_min,
                // Operator withheld storage: no discharge for the envelope.
                power_max: if storage_authorized::withheld(cache, site_id) {
                    guard.power_max.min(0.0)
                } else {
                    guard.power_max
                },
                dt,
            });
            ctrl.current_output()
        }
        None => requested_setpoint,
    };

    let ProtocolBinding::Distribute(d) = &cfg.binding else {
        warn!(device_id = %cfg.device_id, "rebalance task's binding is not distribute; nothing to do");
        return;
    };
    let shares = match dispatch::compute_shares(d, target, site_id, cache) {
        Ok(s) => s,
        Err(_) => return, // hold — same posture as missing cache inputs above
    };
    // Children at their floor or offline can't take their share; the
    // controller must not keep commanding what can't be delivered.
    let delivered: f64 = shares.iter().map(|(_, share)| share).sum();
    if let Some(ctrl) = controller.as_mut()
        && (delivered - target).abs() > 1.0
    {
        ctrl.sync_to_delivered(delivered);
    }
    let Some(plan) = writes.plan(shares) else {
        return;
    };

    let channels = device_channels.read().await;
    let trust = device_trust.read().await;
    match dispatch::write_shares(&plan.writes, &cfg.channel_key, &channels, &trust, creds).await {
        Ok(()) => writes.landed(plan),
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
