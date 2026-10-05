//! Grid recharge toward readiness: between DER events, a guarded battery
//! module nobody is dispatching charges back to its readiness SoC at its
//! recharge rate. The envelope still bounds it (import limit at the POI,
//! each rack's live charge limit).

use crate::asyncapi::types::ProtocolBinding;
use crate::synthetic::{InputCache, as_number};

/// The setpoint the envelope steers toward: the operator's `requested_w`,
/// or `−recharge_power_w` while the module is below readiness, idle (0 W
/// requested) and no DER event is active. Anything missing (either binding
/// field, the module's SoC, the event state) means no charging.
pub fn requested_w(
    binding: &ProtocolBinding,
    device_id: &str,
    requested_w: f64,
    site_id: &str,
    cache: &InputCache,
) -> f64 {
    let ProtocolBinding::Distribute(d) = binding else {
        return requested_w;
    };
    let (Some(readiness), Some(recharge_w)) = (d.readiness_soc_percent, d.recharge_power_w) else {
        return requested_w;
    };
    let reading = |topic: String| cache.get(&topic).and_then(|e| as_number(&e.0));
    let event = reading(format!(
        "sites/{site_id}/devices/der_dispatch/measurements/event_active/none"
    ));
    let soc = reading(format!(
        "sites/{site_id}/devices/{device_id}/measurements/state_of_charge/percent"
    ));
    match (event, soc) {
        (Some(event), Some(soc)) if requested_w == 0.0 && event < 0.5 && soc < readiness => {
            -recharge_w
        }
        _ => requested_w,
    }
}

#[cfg(test)]
#[path = "recharge_test.rs"]
mod tests;
