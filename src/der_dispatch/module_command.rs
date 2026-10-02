//! Hands site distribution's per-module share to the same command handler
//! an operator's command goes through, without touching the broker.

use super::site_distribution::SiteDistributionConfig;
use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::ProtocolBinding;
use crate::dispatch::{self, LastRequestedSetpoints};
use crate::synthetic::InputCache;
use chrono::Utc;
use paho_mqtt::AsyncClient;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::warn;

/// Live device maps the command handler resolves a module's binding from.
pub struct Devices {
    /// Device → channel → binding, refreshed on every spec re-fetch.
    pub channels: Arc<RwLock<HashMap<String, HashMap<String, ProtocolBinding>>>>,
    /// Device → trust material, refreshed alongside `channels`.
    pub trust: Arc<RwLock<HashMap<String, DeviceTrust>>>,
}

/// Hand one command for `module_id` to `dispatch::handle_command`, exactly
/// as an operator's would arrive — acks on events/, last_requested capture,
/// the module's own rebalance-to-racks machinery. Returns whether it was
/// handled (acks published).
#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_one(
    cfg: &SiteDistributionConfig,
    cache: &InputCache,
    mqtt: &AsyncClient,
    devices: &Devices,
    last_requested: &LastRequestedSetpoints,
    module_id: &str,
    share: f64,
) -> bool {
    let topic = format!(
        "sites/{}/devices/{module_id}/commands/set/active_power/watts",
        cfg.site_id
    );
    let command_id = format!(
        "site-dist-{}",
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let payload = format!(
        r#"{{"ts":"{ts}","value":{share},"command_id":"{command_id}"}}"#,
        ts = Utc::now().to_rfc3339(),
    );
    let channels = devices.channels.read().await;
    let trust = devices.trust.read().await;
    let handled = dispatch::handle_command(
        mqtt,
        &cfg.site_id,
        &channels,
        &trust,
        cfg.creds.as_ref(),
        cache,
        last_requested,
        &topic,
        payload.as_bytes(),
    )
    .await;
    match handled {
        Ok(()) => true,
        Err(err) => {
            warn!(%topic, error = %err, "site distribution command not handled");
            false
        }
    }
}
