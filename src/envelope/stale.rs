//! The POI meter has gone quiet: the envelope can't see the limit it's
//! holding, so it doesn't hold blind. Output ramps to 0 W at the module's
//! normal ramp rate (a step to 0 would itself be a jolt at the POI).

use crate::envelope::EnvelopeGuardConfig;
use crate::envelope::control_law::EnvelopeController;
use std::time::Duration;

/// Step `ctrl`'s output toward 0 W by one tick of ramp; the new output.
pub fn ramp_to_zero(
    ctrl: &mut EnvelopeController,
    guard: &EnvelopeGuardConfig,
    dt: Duration,
) -> f64 {
    let u = ctrl.current_output;
    let rated = if u > 0.0 {
        guard.power_max
    } else {
        guard.power_min.abs()
    };
    let step = guard.control.ramp_rate_per_sec * rated * dt.as_secs_f64();
    ctrl.current_output = if u.abs() <= step {
        0.0
    } else {
        u - step * u.signum()
    };
    ctrl.current_output
}

#[cfg(test)]
#[path = "stale_test.rs"]
mod tests;
