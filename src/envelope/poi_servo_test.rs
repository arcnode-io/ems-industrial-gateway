//! POI servo bounds. Limits apply at the POI (+ import), so the bounds move
//! the output by a fraction of measured POI headroom, never by a computed
//! site load. Gains: `APPROACH_GAIN_PER_SEC` toward a limit,
//! `VIOLATION_GAIN_PER_SEC` back from one, and a limit tightening in one step.

use super::poi_servo::{APPROACH_GAIN_PER_SEC, Limit, VIOLATION_GAIN_PER_SEC, bounds};
use std::time::Duration;

const ONE_SEC: Duration = Duration::from_secs(1);

/// A limit that hasn't changed since the last tick.
fn steady(value: f64) -> Limit {
    Limit {
        now: Some(value),
        prev: Some(value),
    }
}

const NONE: Limit = Limit {
    now: None,
    prev: None,
};

#[test]
fn approaching_zero_export_moves_a_fraction_of_the_headroom() {
    // Arrange — 72.8 kW imported, zero export: 72.8 kW of discharge room
    // Act
    let b = bounds(0.0, 72_800.0, NONE, steady(0.0), ONE_SEC);
    // Assert
    assert_eq!(b.headroom_export, 72_800.0);
    assert_eq!(b.ceiling, APPROACH_GAIN_PER_SEC * 72_800.0);
}

#[test]
fn a_measured_export_violation_backs_off_at_the_violation_gain() {
    // Arrange — discharging 100 kW, POI exporting 20 kW, zero export
    // Act
    let b = bounds(100_000.0, -20_000.0, NONE, steady(0.0), ONE_SEC);
    // Assert
    assert_eq!(b.ceiling, 100_000.0 - VIOLATION_GAIN_PER_SEC * 20_000.0);
}

#[test]
fn a_tightened_export_limit_is_taken_off_in_one_step() {
    // Arrange — exporting 1 MW right at a 1 MW limit, which drops to 0
    let export = Limit {
        now: Some(0.0),
        prev: Some(1_000_000.0),
    };
    // Act
    let b = bounds(1_500_000.0, -1_000_000.0, NONE, export, ONE_SEC);
    // Assert — the whole new 1 MW excess comes off now, not over seconds
    assert_eq!(b.ceiling, 500_000.0);
}

#[test]
fn a_measured_import_violation_raises_the_floor() {
    // Arrange — idle, POI importing 150 kW against a 100 kW import limit
    // Act
    let b = bounds(0.0, 150_000.0, steady(100_000.0), NONE, ONE_SEC);
    // Assert — floor moves up into discharge by the violation gain
    assert_eq!(b.headroom_import, -50_000.0);
    assert_eq!(b.floor, VIOLATION_GAIN_PER_SEC * 50_000.0);
}

#[test]
fn no_limits_means_no_bounds() {
    let b = bounds(0.0, 72_800.0, NONE, NONE, ONE_SEC);
    assert_eq!(b.ceiling, f64::INFINITY);
    assert_eq!(b.floor, f64::NEG_INFINITY);
}
