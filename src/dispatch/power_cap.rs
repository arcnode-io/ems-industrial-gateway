//! Power-cap dispatch: one fleet percentage becomes a cap on every child's
//! own power limit (compute_module over its GPUs).

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::{PowerCapBinding, ProtocolBinding};
use crate::config::GatewayCredentials;
use crate::redfish;
use anyhow::{Result, bail};
use futures::{StreamExt, stream};
use std::collections::HashMap;

/// Cap writes in flight at once. Reason: a site has ~800 GPU limits; all at
/// once would open as many connections, and BMCs share hosts in the demo.
const CONCURRENT_WRITES: usize = 32;

/// `(device_id, command key, cap W)` for every child at `percent` of its
/// max, held inside the child's allowable range.
pub fn child_caps(binding: &PowerCapBinding, percent: f64) -> Vec<(String, String, f64)> {
    binding
        .children
        .iter()
        .map(|c| {
            let exact = (c.max_w * percent / 100.0).clamp(c.min_w, c.max_w);
            // Whole watts, the limit's own resolution: drop float noise, then
            // round up so a cap never cuts more than the percentage asked.
            let cap = ((exact * 1e6).round() / 1e6).ceil();
            (c.device_id.clone(), format!("set_{}", c.target), cap)
        })
        .collect()
}

/// Write every child's cap for `percent`, each through its own binding.
///
/// Fails if any child's write fails, naming every one that did; the others
/// still land.
pub async fn dispatch_power_cap(
    binding: &PowerCapBinding,
    percent: f64,
    device_channels: &HashMap<String, HashMap<String, ProtocolBinding>>,
    device_trust: &HashMap<String, DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<()> {
    write_caps(
        child_caps(binding, percent),
        device_channels,
        device_trust,
        creds,
    )
    .await
}

/// Write `(device_id, command key, cap W)` caps, `CONCURRENT_WRITES` at a
/// time.
pub async fn write_caps(
    caps: Vec<(String, String, f64)>,
    device_channels: &HashMap<String, HashMap<String, ProtocolBinding>>,
    device_trust: &HashMap<String, DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<()> {
    let failed: Vec<String> = stream::iter(caps)
        .map(|cap| async move {
            write_one(&cap, device_channels, device_trust, creds)
                .await
                .err()
                .map(|e| format!("{}.{}: {e:#}", cap.0, cap.1))
        })
        .buffer_unordered(CONCURRENT_WRITES)
        .filter_map(std::future::ready)
        .collect()
        .await;
    if !failed.is_empty() {
        bail!("{} cap writes failed: {}", failed.len(), failed.join("; "));
    }
    Ok(())
}

/// One child's cap, through the binding its own command declares.
async fn write_one(
    (device_id, key, cap): &(String, String, f64),
    device_channels: &HashMap<String, HashMap<String, ProtocolBinding>>,
    device_trust: &HashMap<String, DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<()> {
    match device_channels.get(device_id).and_then(|c| c.get(key)) {
        Some(ProtocolBinding::Redfish(b)) => {
            redfish::write::write_setpoint(b, *cap, device_trust.get(device_id), creds).await
        }
        Some(_) => bail!("power cap child {device_id}.{key} isn't a Redfish binding"),
        None => bail!("power cap child {device_id}.{key} isn't in the spec"),
    }
}

#[cfg(test)]
#[path = "power_cap_test.rs"]
mod tests;
