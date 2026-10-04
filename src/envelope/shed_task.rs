//! One task per envelope-guarded `power_cap` command (a compute_module):
//! ticks the shed controller on each fresh POI reading and writes the fleet
//! cap on change.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::{PowerCapBinding, ProtocolBinding};
use crate::config::GatewayCredentials;
use crate::dispatch::{LastRequestedSetpoints, power_cap};
use crate::envelope::inputs::EnvelopeConfig;
use crate::envelope::shed::{ShedController, ShedTick};
use crate::inputs::substitute_site_id;
use crate::synthetic::{InputCache, as_number};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Live device maps a cap write resolves each child's binding from.
pub type Devices = (
    Arc<RwLock<HashMap<String, HashMap<String, ProtocolBinding>>>>,
    Arc<RwLock<HashMap<String, DeviceTrust>>>,
);

/// One compute module's guarded power cap.
pub struct CapModule {
    /// The compute_module.
    pub device_id: String,
    /// `{verb}_{target}`: where the operator's own fleet cap is recorded.
    pub channel_key: String,
    /// Children and their ranges.
    pub binding: PowerCapBinding,
}

/// What one shed task runs on: every guarded power cap behind one POI.
///
/// Reason: one controller per POI, not per module. Each module answering the
/// whole site's import would cut it once per module and drive the site into
/// export; one site-wide percentage covers it once.
pub struct ShedTaskConfig {
    /// The modules this POI's shed drives.
    pub modules: Vec<CapModule>,
    /// Dwell, margin, ramp.
    pub control: EnvelopeConfig,
    /// POI `active_power` topic, `{site_id}` substituted.
    pub poi_topic: String,
    /// `import_limit` topic, `{site_id}` substituted.
    pub import_limit_topic: String,
    /// `export_limit` topic, `{site_id}` substituted.
    pub export_limit_topic: String,
    /// mTLS material for the BMC writes.
    pub creds: Option<GatewayCredentials>,
}

impl ShedTaskConfig {
    /// `Some` when the binding carries the envelope guard (the DTM enabled
    /// compute shedding); a plain power cap is operator-only.
    pub fn from_binding(
        device_id: &str,
        channel_key: &str,
        b: &PowerCapBinding,
        site_id: &str,
        creds: Option<GatewayCredentials>,
    ) -> Option<Self> {
        Some(Self {
            modules: vec![CapModule {
                device_id: device_id.to_string(),
                channel_key: channel_key.to_string(),
                binding: b.clone(),
            }],
            control: EnvelopeConfig {
                ramp_rate_per_sec: b.ramp_rate_per_sec?,
                hysteresis_margin: b.hysteresis_margin?,
                hysteresis_dwell: Duration::from_secs_f64(b.hysteresis_dwell_secs?),
            },
            poi_topic: substitute_site_id(b.poi_active_power_topic.as_ref()?, site_id),
            import_limit_topic: substitute_site_id(b.import_limit_topic.as_ref()?, site_id),
            export_limit_topic: substitute_site_id(b.export_limit_topic.as_ref()?, site_id),
            creds,
        })
    }

    /// Merge per-module configs into one per POI (and envelope); the first
    /// module's dwell, margin and ramp govern its group.
    pub fn per_poi(configs: Vec<Self>) -> Vec<Self> {
        let mut groups: Vec<Self> = Vec::new();
        for c in configs {
            let same = |g: &&mut Self| {
                (&g.poi_topic, &g.import_limit_topic, &g.export_limit_topic)
                    == (&c.poi_topic, &c.import_limit_topic, &c.export_limit_topic)
            };
            match groups.iter_mut().find(same) {
                Some(g) => g.modules.extend(c.modules),
                None => groups.push(c),
            }
        }
        groups
    }
}

/// Spawn the shed loop; exits when `cancel` fires.
pub fn spawn(
    cfg: ShedTaskConfig,
    cache: InputCache,
    last_requested: LastRequestedSetpoints,
    devices: Devices,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut controller = ShedController::new(cfg.control);
        let mut ticker = interval(Duration::from_secs(1));
        let mut last_poi: Option<Instant> = None;
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    let Some(t) = inputs(&cfg, &cache, &mut last_poi) else {
                        continue; // hold — an input missing or the POI reading not new
                    };
                    let Some(percent) = controller.tick(&t) else { continue };
                    info!(poi = %cfg.poi_topic, percent, modules = cfg.modules.len(), "compute shed: fleet cap");
                    let caps = module_caps(&cfg, percent, &last_requested).await;
                    let (channels, trust) = (devices.0.read().await, devices.1.read().await);
                    match power_cap::write_caps(caps, &channels, &trust, cfg.creds.as_ref()).await {
                        Ok(()) => controller.confirm(percent),
                        Err(e) => warn!(poi = %cfg.poi_topic, error = format!("{e:#}"), "compute shed write failed"),
                    }
                }
            }
        }
    })
}

/// This tick's inputs, or `None` to hold. Only a POI reading no earlier tick
/// used counts, with its real age as `dt`.
fn inputs(
    cfg: &ShedTaskConfig,
    cache: &InputCache,
    last_poi: &mut Option<Instant>,
) -> Option<ShedTick> {
    let number = |topic: &str| cache.get(topic).and_then(|e| Some((as_number(&e.0)?, e.1)));
    let (poi, received_at) = number(&cfg.poi_topic)?;
    let (import_limit, _) = number(&cfg.import_limit_topic)?;
    let (export_limit, _) = number(&cfg.export_limit_topic)?;
    let dt = match *last_poi {
        Some(prev) if received_at <= prev => return None,
        Some(prev) => received_at - prev,
        None => Duration::from_secs(1),
    };
    *last_poi = Some(received_at);
    let children = || cfg.modules.iter().flat_map(|m| &m.binding.children);
    Some(ShedTick {
        poi_active_power: poi,
        import_limit,
        export_limit,
        // Shedding restores to full; each module's own operator cap still
        // limits its GPUs when written (`module_caps`).
        requested_percent: 100.0,
        fleet_max_w: children().map(|c| c.max_w).sum(),
        fleet_min_w: children().map(|c| c.min_w).sum(),
        dt,
    })
}

/// Every module's caps at the site's `percent`, never above the cap its
/// operator set on that module.
async fn module_caps(
    cfg: &ShedTaskConfig,
    percent: f64,
    last_requested: &LastRequestedSetpoints,
) -> Vec<(String, String, f64)> {
    let requested = last_requested.read().await;
    cfg.modules
        .iter()
        .flat_map(|m| {
            let operator = requested
                .get(&m.device_id)
                .and_then(|c| c.get(&m.channel_key))
                .copied()
                .unwrap_or(100.0);
            power_cap::child_caps(&m.binding, percent.min(operator))
        })
        .collect()
}
