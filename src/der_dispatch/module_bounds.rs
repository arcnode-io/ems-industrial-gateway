//! Which devices site distribution splits across: every distribute-parent
//! (`bess_module`) with resolvable bounds and a cached state_of_charge.

use super::site_distribution::CHANNEL_KEY;
use crate::asyncapi::types::ProtocolBinding;
use crate::dispatch::allocation::{ChildCapacity, OperatingState};
use crate::synthetic::InputCache;
use std::collections::HashMap;

/// Every distribute-parent device (a `bess_module`) with a resolvable
/// power_min/power_max (from its own Distribute binding) and cached
/// state_of_charge. `None` if any known module's state_of_charge isn't
/// cached yet — hold, same posture as everywhere else; an empty (but
/// `Some`) result means there are simply no modules yet.
pub(super) fn modules_with_bounds(
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
#[path = "module_bounds_test.rs"]
mod tests;
