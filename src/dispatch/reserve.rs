//! The reserve floor a battery module's racks don't discharge below: the
//! supplier's (warranty, from the DTM) or the operator's runtime reserve
//! (`der_dispatch/operator_reserve`), whichever is higher. The supplier
//! floor can't be undercut by construction.

use crate::asyncapi::types::DistributeBinding;
use crate::inputs::substitute_site_id;
use crate::synthetic::{InputCache, as_number};

/// The effective floor, percent SoC; `None` when neither applies.
pub fn effective_floor_percent(
    binding: &DistributeBinding,
    site_id: &str,
    cache: &InputCache,
) -> Option<f64> {
    let operator = operator_reserve_percent(binding, site_id, cache);
    match (binding.state_of_charge_floor_percent, operator) {
        (Some(supplier), Some(operator)) => Some(supplier.max(operator)),
        (supplier, operator) => supplier.or(operator),
    }
}

/// The operator's reserve as a percent of site capacity, at most 100 (a
/// reserve beyond installed energy means "never discharge"). `None` until
/// both the capacity and a published reserve are known.
fn operator_reserve_percent(
    binding: &DistributeBinding,
    site_id: &str,
    cache: &InputCache,
) -> Option<f64> {
    let capacity_wh = binding.site_capacity_wh.filter(|c| *c > 0.0)?;
    let topic = substitute_site_id(binding.operator_reserve_topic.as_ref()?, site_id);
    let reserve_wh = cache.get(&topic).and_then(|e| as_number(&e.0))?;
    Some((reserve_wh / capacity_wh * 100.0).clamp(0.0, 100.0))
}

#[cfg(test)]
#[path = "reserve_test.rs"]
mod tests;
