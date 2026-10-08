//! A quiet POI meter ramps the module to 0 W at its normal rate.

use super::ramp_to_zero;
use crate::envelope::EnvelopeGuardConfig;
use crate::envelope::control_law::{EnvelopeConfig, EnvelopeController};
use std::time::Duration;

const SECOND: Duration = Duration::from_secs(1);

fn config() -> EnvelopeConfig {
    EnvelopeConfig {
        ramp_rate_per_sec: 0.10,
        hysteresis_margin: 0.05,
        hysteresis_dwell: Duration::from_secs(30),
    }
}

/// 1 MW module, 10%/s ramp.
fn guard() -> EnvelopeGuardConfig {
    EnvelopeGuardConfig {
        control: config(),
        power_min: -1_000_000.0,
        power_max: 1_000_000.0,
        import_limit_topic: String::new(),
        export_limit_topic: String::new(),
        active_power_topic: String::new(),
        poi_active_power_topic: None,
    }
}

#[test]
fn discharge_ramps_down_to_zero_and_stays() {
    // Arrange
    let mut ctrl = EnvelopeController::new(config(), 250_000.0);
    let g = guard();
    // Act + Assert — 100 kW per second, then 0
    assert_eq!(ramp_to_zero(&mut ctrl, &g, SECOND), 150_000.0);
    assert_eq!(ramp_to_zero(&mut ctrl, &g, SECOND), 50_000.0);
    assert_eq!(ramp_to_zero(&mut ctrl, &g, SECOND), 0.0);
    assert_eq!(ramp_to_zero(&mut ctrl, &g, SECOND), 0.0);
}

#[test]
fn charging_ramps_up_to_zero() {
    let mut ctrl = EnvelopeController::new(config(), -150_000.0);
    assert_eq!(ramp_to_zero(&mut ctrl, &guard(), SECOND), -50_000.0);
    assert_eq!(ramp_to_zero(&mut ctrl, &guard(), SECOND), 0.0);
}
