//! Discharge a POI's guarded batteries could still add: their rated
//! discharge, as far as their racks can deliver it (state, reserve floor,
//! live limits), minus what they deliver now. With none, the compute shed
//! starts at once instead of waiting for storage to answer.

use crate::asyncapi::types::DistributeBinding;
use crate::dispatch::compute_shares;
use crate::inputs::substitute_site_id;
use crate::synthetic::{InputCache, as_number};
use std::collections::HashSet;

/// Spare discharge summed over the guarded battery `modules` servoing on
/// `poi_topic` (substituted), W.
pub fn spare_w(
    modules: &[DistributeBinding],
    poi_topic: &str,
    site_id: &str,
    cache: &InputCache,
    locked: &HashSet<String>,
) -> f64 {
    modules
        .iter()
        .filter(|m| {
            m.poi_active_power_topic
                .as_ref()
                .is_some_and(|t| substitute_site_id(t, site_id) == poi_topic)
        })
        .map(|m| module_spare_w(m, site_id, cache, locked))
        .sum()
}

/// Spare discharge of one module, W; 0 when it has none or a reading is
/// missing.
pub fn module_spare_w(
    m: &DistributeBinding,
    site_id: &str,
    cache: &InputCache,
    locked: &HashSet<String>,
) -> f64 {
    let Some(topic) = m.active_power_topic.as_ref() else {
        return 0.0;
    };
    let topic = substitute_site_id(topic, site_id);
    let Some(active_w) = cache.get(&topic).and_then(|e| as_number(&e.0)) else {
        return 0.0;
    };
    let rated = m
        .power_max
        .unwrap_or_else(|| m.children.iter().map(|c| c.power_max).sum());
    // Reason: the same allocation the envelope writes with, so racks at the
    // reserve floor or faulted count for nothing.
    let deliverable: f64 = compute_shares(m, rated, site_id, cache, locked)
        .map(|shares| shares.iter().map(|(_, w)| w).sum())
        .unwrap_or(0.0);
    (deliverable - active_w).max(0.0)
}

#[cfg(test)]
#[path = "storage_spare_test.rs"]
mod tests;
