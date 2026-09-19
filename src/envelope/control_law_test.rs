//! Unit tests for the envelope control law — ramp rate, hysteresis, clamp.

use super::control_law::{EnvelopeConfig, EnvelopeController, EnvelopeTick};
use std::time::Duration;

/// power-engineer's defaults: 10%/sec ramp, 5% margin, 30s dwell.
fn config() -> EnvelopeConfig {
    EnvelopeConfig {
        ramp_rate_per_sec: 0.10,
        hysteresis_margin: 0.05,
        hysteresis_dwell: Duration::from_secs(30),
    }
}

const RATED_POWER: f64 = 4_000_000.0;
const ONE_SEC: Duration = Duration::from_secs(1);

fn tick(
    import_limit: Option<f64>,
    export_limit: Option<f64>,
    active_power: f64,
    requested_setpoint: f64,
) -> EnvelopeTick {
    EnvelopeTick {
        import_limit,
        export_limit,
        active_power,
        requested_setpoint,
        rated_power: RATED_POWER,
        dt: ONE_SEC,
    }
}

#[test]
fn normal_mode_tracks_requested_setpoint_when_unconstrained() {
    // Arrange — generous limits, well within bounds
    let mut ctrl = EnvelopeController::new(config(), 0.0);
    // Act
    let out = ctrl.tick(tick(
        Some(3_000_000.0),
        Some(3_000_000.0),
        500_000.0,
        1_000_000.0,
    ));
    // Assert
    assert_eq!(out, Some(1_000_000.0));
}

#[test]
fn no_limits_at_all_never_constrains() {
    // Arrange — neither limit published yet (upstream fix not landed)
    let mut ctrl = EnvelopeController::new(config(), 0.0);
    // Act
    let out = ctrl.tick(tick(None, None, 3_900_000.0, 3_999_999.0));
    // Assert — nothing to clamp against, request passes through
    assert_eq!(out, Some(3_999_999.0));
}

#[test]
fn violated_import_ceiling_clamps_immediately_no_dwell() {
    // Arrange — gateway believes the device is still below the ceiling
    let mut ctrl = EnvelopeController::new(config(), 500_000.0);
    // Act — import_limit=1_000_000, active_power=1_000_000 -> headroom=0, request exceeds it
    let out = ctrl.tick(tick(Some(1_000_000.0), None, 1_000_000.0, 1_500_000.0));
    // Assert — clamped to the ceiling immediately, no dwell required
    assert_eq!(out, Some(1_000_000.0));
}

#[test]
fn violated_export_floor_clamps_immediately() {
    // Arrange — gateway believes the device is still above the (negative) floor
    let mut ctrl = EnvelopeController::new(config(), -500_000.0);
    // Act — export_limit=1_000_000 -> floor=-1_000_000, requesting further export
    let out = ctrl.tick(tick(None, Some(1_000_000.0), -1_000_000.0, -1_500_000.0));
    // Assert
    assert_eq!(out, Some(-1_000_000.0));
}

#[test]
fn constrained_holds_output_until_dwell_satisfied() {
    // Arrange — enter constrained
    let mut ctrl = EnvelopeController::new(config(), 1_000_000.0);
    ctrl.tick(tick(Some(1_000_000.0), None, 1_000_000.0, 1_500_000.0));
    // Act — limit relaxes with generous margin, but only 10s of the 30s dwell
    let mut last = None;
    for _ in 0..10 {
        last = ctrl.tick(tick(Some(2_000_000.0), None, 1_000_000.0, 1_500_000.0));
    }
    // Assert — still holding, no write (dwell not yet satisfied)
    assert_eq!(last, None);
}

#[test]
fn constrained_ramps_only_after_dwell_satisfied() {
    // Arrange — enter constrained at ceiling=1_000_000
    let mut ctrl = EnvelopeController::new(config(), 1_000_000.0);
    ctrl.tick(tick(Some(1_000_000.0), None, 1_000_000.0, 1_500_000.0));
    // Act — limit relaxes to 2_000_000 (headroom = 1_000_000, well above the
    // 5% * 4MW = 200_000 margin); hold for exactly 30 ticks to satisfy dwell
    let mut out = None;
    for _ in 0..30 {
        out = ctrl.tick(tick(Some(2_000_000.0), None, 1_000_000.0, 1_500_000.0));
    }
    // Assert — the 30th tick starts ramping: max_step = 0.10 * 4_000_000 * 1s = 400_000
    assert_eq!(out, Some(1_000_000.0 + 400_000.0));
}

#[test]
fn dwell_resets_if_margin_lost_before_satisfied() {
    // Arrange — enter constrained
    let mut ctrl = EnvelopeController::new(config(), 1_000_000.0);
    ctrl.tick(tick(Some(1_000_000.0), None, 1_000_000.0, 1_500_000.0));
    // Act — 20 good ticks (within margin), then one tick back at the ceiling
    // (headroom back to 0, a fresh violation resets dwell), then 20 more
    // good ticks — should NOT have accumulated 30s yet.
    for _ in 0..20 {
        ctrl.tick(tick(Some(2_000_000.0), None, 1_000_000.0, 1_500_000.0));
    }
    ctrl.tick(tick(Some(1_000_000.0), None, 1_000_000.0, 1_500_000.0));
    let mut out = None;
    for _ in 0..20 {
        out = ctrl.tick(tick(Some(2_000_000.0), None, 1_000_000.0, 1_500_000.0));
    }
    // Assert — only 20 consecutive good ticks since the reset, dwell not satisfied
    assert_eq!(out, None);
}

#[test]
fn ramping_steps_at_configured_rate_then_settles_at_target() {
    // Arrange — enter constrained, hold 29 of the 30s dwell.
    let mut ctrl = EnvelopeController::new(config(), 1_000_000.0);
    ctrl.tick(tick(Some(1_000_000.0), None, 1_000_000.0, 1_200_000.0));
    for _ in 0..29 {
        ctrl.tick(tick(Some(2_000_000.0), None, 1_000_000.0, 1_200_000.0));
    }
    // Act — the 30th tick closes the dwell AND starts ramping in the same
    // call; target (1_200_000) is only 200_000 away, less than the 400_000
    // max step, so it settles exactly on target immediately.
    let out = ctrl.tick(tick(Some(2_000_000.0), None, 1_000_000.0, 1_200_000.0));
    // Assert
    assert_eq!(out, Some(1_200_000.0));
    // Act — a following tick with the same inputs, now settled to Normal:
    // unchanged, no write.
    let out2 = ctrl.tick(tick(Some(2_000_000.0), None, 1_000_000.0, 1_200_000.0));
    assert_eq!(out2, None);
}

#[test]
fn fresh_violation_mid_ramp_immediately_re_clamps() {
    // Arrange — get into Ramping mode
    let mut ctrl = EnvelopeController::new(config(), 1_000_000.0);
    ctrl.tick(tick(Some(1_000_000.0), None, 1_000_000.0, 3_000_000.0));
    for _ in 0..30 {
        ctrl.tick(tick(Some(2_000_000.0), None, 1_000_000.0, 3_000_000.0));
    }
    let ramped = ctrl
        .tick(tick(Some(2_000_000.0), None, 1_000_000.0, 3_000_000.0))
        .unwrap();
    assert!(ramped > 1_000_000.0 && ramped < 3_000_000.0);
    // Act — the limit tightens back down hard mid-ramp
    let out = ctrl.tick(tick(Some(900_000.0), None, ramped, 3_000_000.0));
    // Assert — instant re-clamp to the new, tighter ceiling
    assert_eq!(out, Some(900_000.0));
}
