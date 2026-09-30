//! Unit tests for the envelope control law — ramp rate, hysteresis, clamp.
//! Sign convention under test: positive active_power = discharge (export),
//! negative = charge (import).

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

/// Symmetric nameplate bounds, matching bess_rack's own bounds convention.
const POWER_MIN: f64 = -4_000_000.0;
const POWER_MAX: f64 = 4_000_000.0;
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
        poi_active_power: None,
        poi_fresh: true,
        hold_approach: false,
        power_min: POWER_MIN,
        power_max: POWER_MAX,
        dt: ONE_SEC,
    }
}

#[test]
fn current_output_reflects_last_committed_value_even_when_tick_returns_none() {
    // Arrange — a tick that changes the output, then one that doesn't.
    let mut ctrl = EnvelopeController::new(config(), 0.0);
    let changed = ctrl.tick(tick(Some(10_000_000.0), Some(10_000_000.0), 0.0, 500_000.0));
    assert_eq!(changed, Some(500_000.0));

    // Act — identical inputs: tick() reports no change...
    let unchanged = ctrl.tick(tick(Some(10_000_000.0), Some(10_000_000.0), 0.0, 500_000.0));

    // Assert — ...but current_output still reflects the real committed value.
    assert_eq!(unchanged, None);
    assert_eq!(ctrl.current_output(), 500_000.0);
}

#[test]
fn normal_mode_tracks_requested_setpoint_when_unconstrained() {
    let mut ctrl = EnvelopeController::new(config(), 0.0);
    let out = ctrl.tick(tick(
        Some(3_000_000.0),
        Some(3_000_000.0),
        500_000.0,
        1_000_000.0,
    ));
    assert_eq!(out, Some(1_000_000.0));
}

#[test]
fn no_limits_at_all_never_constrains() {
    let mut ctrl = EnvelopeController::new(config(), 0.0);
    let out = ctrl.tick(tick(None, None, 3_900_000.0, 3_999_999.0));
    assert_eq!(out, Some(3_999_999.0));
}

#[test]
fn import_limit_does_not_cap_discharge() {
    // Arrange — discharging 100kW against a tight 50kW import limit. Discharge
    // reduces site import, so the import limit has nothing to say about it.
    let mut ctrl = EnvelopeController::new(config(), 100_000.0);
    // Act
    let out = ctrl.tick(tick(Some(50_000.0), None, 100_000.0, 200_000.0));
    // Assert — request passes through unclamped
    assert_eq!(out, Some(200_000.0));
}

#[test]
fn export_limit_caps_discharge_immediately_no_dwell() {
    // Arrange — gateway believes the device is still below the ceiling
    let mut ctrl = EnvelopeController::new(config(), 500_000.0);
    // Act — export_limit=1MW, discharging 1MW -> headroom 0, request exceeds it
    let out = ctrl.tick(tick(None, Some(1_000_000.0), 1_000_000.0, 1_500_000.0));
    // Assert — clamped to the export limit immediately
    assert_eq!(out, Some(1_000_000.0));
}

#[test]
fn import_limit_caps_charge_immediately() {
    // Arrange
    let mut ctrl = EnvelopeController::new(config(), -500_000.0);
    // Act — import_limit=1MW -> floor=-1MW, charging 1MW, requesting more
    let out = ctrl.tick(tick(Some(1_000_000.0), None, -1_000_000.0, -1_500_000.0));
    // Assert
    assert_eq!(out, Some(-1_000_000.0));
}

#[test]
fn constrained_holds_output_until_dwell_satisfied() {
    // Arrange — enter constrained against the export limit
    let mut ctrl = EnvelopeController::new(config(), 1_000_000.0);
    ctrl.tick(tick(None, Some(1_000_000.0), 1_000_000.0, 1_500_000.0));
    // Act — limit relaxes with generous margin, but only 10s of the 30s dwell
    let mut last = None;
    for _ in 0..10 {
        last = ctrl.tick(tick(None, Some(2_000_000.0), 1_000_000.0, 1_500_000.0));
    }
    // Assert — still holding, no write
    assert_eq!(last, None);
}

#[test]
fn constrained_ramps_only_after_dwell_satisfied() {
    // Arrange — enter constrained at export ceiling=1MW
    let mut ctrl = EnvelopeController::new(config(), 1_000_000.0);
    ctrl.tick(tick(None, Some(1_000_000.0), 1_000_000.0, 1_500_000.0));
    // Act — limit relaxes to 2MW (headroom 1MW > 5% * 4MW margin); 30 ticks
    let mut out = None;
    for _ in 0..30 {
        out = ctrl.tick(tick(None, Some(2_000_000.0), 1_000_000.0, 1_500_000.0));
    }
    // Assert — 30th tick starts ramping: max_step = 0.10 * 4MW * 1s = 400kW
    assert_eq!(out, Some(1_000_000.0 + 400_000.0));
}

#[test]
fn dwell_resets_if_margin_lost_before_satisfied() {
    // Arrange — enter constrained
    let mut ctrl = EnvelopeController::new(config(), 1_000_000.0);
    ctrl.tick(tick(None, Some(1_000_000.0), 1_000_000.0, 1_500_000.0));
    // Act — 20 good ticks, one fresh violation (resets dwell), 20 more
    for _ in 0..20 {
        ctrl.tick(tick(None, Some(2_000_000.0), 1_000_000.0, 1_500_000.0));
    }
    ctrl.tick(tick(None, Some(1_000_000.0), 1_000_000.0, 1_500_000.0));
    let mut out = None;
    for _ in 0..20 {
        out = ctrl.tick(tick(None, Some(2_000_000.0), 1_000_000.0, 1_500_000.0));
    }
    // Assert — only 20 consecutive good ticks since the reset
    assert_eq!(out, None);
}

#[test]
fn ramping_steps_at_configured_rate_then_settles_at_target() {
    // Arrange — enter constrained, hold 29 of the 30s dwell
    let mut ctrl = EnvelopeController::new(config(), 1_000_000.0);
    ctrl.tick(tick(None, Some(1_000_000.0), 1_000_000.0, 1_200_000.0));
    for _ in 0..29 {
        ctrl.tick(tick(None, Some(2_000_000.0), 1_000_000.0, 1_200_000.0));
    }
    // Act — 30th tick closes dwell and ramps; target 200kW away < 400kW step
    let out = ctrl.tick(tick(None, Some(2_000_000.0), 1_000_000.0, 1_200_000.0));
    // Assert — settles exactly on target, then no further writes
    assert_eq!(out, Some(1_200_000.0));
    let out2 = ctrl.tick(tick(None, Some(2_000_000.0), 1_000_000.0, 1_200_000.0));
    assert_eq!(out2, None);
}

#[test]
fn fresh_violation_mid_ramp_immediately_re_clamps() {
    // Arrange — get into Ramping mode
    let mut ctrl = EnvelopeController::new(config(), 1_000_000.0);
    ctrl.tick(tick(None, Some(1_000_000.0), 1_000_000.0, 3_000_000.0));
    for _ in 0..30 {
        ctrl.tick(tick(None, Some(2_000_000.0), 1_000_000.0, 3_000_000.0));
    }
    let ramped = ctrl
        .tick(tick(None, Some(2_000_000.0), 1_000_000.0, 3_000_000.0))
        .unwrap();
    assert!(ramped > 1_000_000.0 && ramped < 3_000_000.0);
    // Act — the export limit tightens hard mid-ramp
    let out = ctrl.tick(tick(None, Some(900_000.0), ramped, 3_000_000.0));
    // Assert — instant re-clamp to the new ceiling
    assert_eq!(out, Some(900_000.0));
}

#[test]
fn ramp_step_is_direction_dependent_on_asymmetric_bounds() {
    // Arrange — small charge capacity, large discharge capacity: ramping
    // toward discharge uses power_max, toward charge uses |power_min|.
    let asymmetric_tick =
        |import_limit, export_limit, active_power, requested_setpoint| EnvelopeTick {
            import_limit,
            export_limit,
            active_power,
            requested_setpoint,
            poi_active_power: None,
            poi_fresh: true,
            hold_approach: false,
            power_min: -1_000_000.0,
            power_max: 4_000_000.0,
            dt: ONE_SEC,
        };

    // Act — ramp toward discharge: max_step = 0.10 * 4MW = 400kW
    let mut discharge = EnvelopeController::new(config(), 0.0);
    discharge.tick(asymmetric_tick(None, Some(0.0), 0.0, 1_000_000.0));
    for _ in 0..29 {
        discharge.tick(asymmetric_tick(None, Some(2_000_000.0), 0.0, 1_000_000.0));
    }
    let discharge_step = discharge
        .tick(asymmetric_tick(None, Some(2_000_000.0), 0.0, 1_000_000.0))
        .unwrap();
    assert!((discharge_step - 400_000.0).abs() < f64::EPSILON);

    // Act — ramp toward charge: max_step = 0.10 * 1MW = 100kW
    let mut charge = EnvelopeController::new(config(), 0.0);
    charge.tick(asymmetric_tick(Some(0.0), None, 0.0, -1_000_000.0));
    for _ in 0..29 {
        charge.tick(asymmetric_tick(Some(2_000_000.0), None, 0.0, -1_000_000.0));
    }
    let charge_step = charge
        .tick(asymmetric_tick(Some(2_000_000.0), None, 0.0, -1_000_000.0))
        .unwrap();
    assert!((charge_step - -100_000.0).abs() < f64::EPSILON);
}
