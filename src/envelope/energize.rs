//! IEEE 2030.5 `opModEnergize`: a control in force with energize false
//! means the DER must cease to energize.
//!
//! Reason: 0 W is an approximation, not conformance. Ceasing to energize is
//! opening the contactor; 0 W leaves the plant connected and synchronized.
//! The rack bindings expose no disconnect, so holding every module at 0 W is
//! the strongest action available, and strictly safer than ignoring it.
//!
//! Releasing the hold snaps straight back to the target. That's only
//! acceptable because the plant never leaves service. A real disconnect
//! brings IEEE 1547-2018 enter-service obligations with it (a ramp, by
//! default over 300 s in steps of at most 20% of rating, and 5 minutes of
//! healthy voltage and frequency before reconnecting after a trip), so the
//! disconnect and that ramp land in the same change or not at all.
//!
//! Only an explicit `false` acts. `energize_enabled` is absent on any site
//! that has never received an energize control, so missing must mean
//! permissive here, the inverse of every other gate.

use crate::synthetic::{InputCache, as_number};

/// `target_w`, or 0 W while der_dispatch's `energize_enabled` is `false`.
pub fn target_w(target_w: f64, site_id: &str, cache: &InputCache) -> f64 {
    let topic = format!("sites/{site_id}/devices/der_dispatch/measurements/energize_enabled/none");
    match cache.get(&topic).and_then(|e| as_number(&e.0)) {
        Some(energize) if energize < 0.5 => 0.0,
        _ => target_w,
    }
}

#[cfg(test)]
#[path = "energize_test.rs"]
mod tests;
