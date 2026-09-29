//! Unit tests for the rate-limited site-load estimate.

use super::site_load::SiteLoadEstimate;
use std::time::Duration;

const ONE_SEC: Duration = Duration::from_secs(1);

#[test]
fn first_reading_is_taken_as_is() {
    let mut est = SiteLoadEstimate::new(50_000.0);
    assert_eq!(est.update(1_120_000.0, ONE_SEC), 1_120_000.0);
}

#[test]
fn a_battery_step_seen_through_a_lagging_meter_moves_the_estimate_only_by_the_ramp() {
    // Arrange — steady 1.12 MW load
    let mut est = SiteLoadEstimate::new(50_000.0);
    est.update(1_120_000.0, ONE_SEC);
    // Act — battery steps +1.12 MW before the meter sees it: raw reads 2.24 MW
    let during_skew = est.update(2_240_000.0, ONE_SEC);
    // Assert — moved 50 kW, not 1.12 MW
    assert_eq!(during_skew, 1_170_000.0);
}

#[test]
fn estimate_returns_once_the_meter_catches_up() {
    let mut est = SiteLoadEstimate::new(50_000.0);
    est.update(1_120_000.0, ONE_SEC);
    est.update(2_240_000.0, ONE_SEC);
    // Meter caught up: raw is the true load again
    assert_eq!(est.update(1_120_000.0, ONE_SEC), 1_120_000.0);
}

#[test]
fn a_real_load_change_is_tracked_at_the_ramp_rate() {
    let mut est = SiteLoadEstimate::new(50_000.0);
    est.update(1_000_000.0, ONE_SEC);
    // Load drops 200 kW and stays there
    let values: Vec<f64> = (0..5).map(|_| est.update(800_000.0, ONE_SEC)).collect();
    assert_eq!(
        values,
        vec![950_000.0, 900_000.0, 850_000.0, 800_000.0, 800_000.0]
    );
}

#[test]
fn ramp_scales_with_elapsed_time() {
    let mut est = SiteLoadEstimate::new(50_000.0);
    est.update(0.0, ONE_SEC);
    assert_eq!(
        est.update(1_000_000.0, Duration::from_millis(500)),
        25_000.0
    );
}
