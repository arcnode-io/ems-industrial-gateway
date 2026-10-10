//! Which devices site distribution splits across: every distribute-parent
//! (`bess_module`) with resolvable bounds and a cached state_of_charge.

use super::site_distribution::CHANNEL_KEY;
use crate::asyncapi::types::ProtocolBinding;
use crate::dispatch::allocation::{ChildCapacity, OperatingState};
use crate::dispatch::rack_limits;
use crate::synthetic::{InputCache, as_number};
use std::collections::{HashMap, HashSet};

/// Every distribute-parent device (a `bess_module`) not locked out, with a resolvable
/// power_min/power_max (from its own Distribute binding, capped by its
/// racks' live limits) and cached
/// state_of_charge. `None` if any known module's state_of_charge isn't
/// cached yet — hold, same posture as everywhere else; an empty (but
/// `Some`) result means there are simply no modules yet.
pub(super) fn modules_with_bounds(
    channels: &HashMap<String, HashMap<String, ProtocolBinding>>,
    cache: &InputCache,
    site_id: &str,
    target: f64,
    locked: &HashSet<String>,
) -> Option<Vec<ChildCapacity>> {
    let mut modules = Vec::new();
    for (device_id, commands) in channels {
        let Some(ProtocolBinding::Distribute(d)) = commands.get(CHANNEL_KEY) else {
            continue;
        };
        if locked.contains(device_id) {
            continue;
        }
        let (Some(power_min), Some(power_max)) = (d.power_min, d.power_max) else {
            continue;
        };
        let soc_topic =
            format!("sites/{site_id}/devices/{device_id}/measurements/state_of_charge/percent");
        let state_of_charge = cache.get(&soc_topic).and_then(|e| as_number(&e.0))?;
        let static_w = if target < 0.0 {
            power_min.abs()
        } else {
            power_max
        };
        // Reason: racks derate at the SoC ends; the module can't give more
        // than its racks can right now.
        let racks_w: f64 = d
            .children
            .iter()
            .filter(|c| !locked.contains(&c.device_id))
            .map(|c| rack_limits::headroom(c, target, site_id, cache))
            .sum();
        let headroom = if d.children.is_empty() {
            static_w
        } else {
            static_w.min(racks_w)
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

#[cfg(test)]
#[path = "module_bounds_loto_test.rs"]
mod loto_tests;
