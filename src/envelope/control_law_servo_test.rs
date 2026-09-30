//! Closed-loop check of the POI envelope against a lagging meter. High-risk:
//! a law that feeds on the meter's lag exports a large share of site load
//! under a zero-export envelope for seconds whenever the battery ramps, which
//! on real hardware is a reverse-power trip. The meter reads the battery's
//! step ~2 s late here.

use super::control_law::{EnvelopeConfig, EnvelopeController, EnvelopeTick};
use std::collections::VecDeque;
use std::time::Duration;

/// Site load.
const LOAD_W: f64 = 1_120_000.0;
/// Requested discharge, well past the load, so zero-export binds.
const REQUESTED_W: f64 = 1_200_000.0;
const POWER_MAX: f64 = 4_000_000.0;
/// Meter lag in ticks (1 s each).
const METER_LAG_TICKS: usize = 2;
const TICKS: usize = 150;

fn config() -> EnvelopeConfig {
    EnvelopeConfig {
        ramp_rate_per_sec: 0.10,
        hysteresis_margin: 0.05,
        hysteresis_dwell: Duration::from_secs(30),
    }
}

/// Run the loop: battery holds each command instantly, the meter shows the
/// POI as it was `METER_LAG_TICKS` ago. `load(tick)` is the site load.
/// Returns the true POI (+ import) each tick.
fn simulate(import_limit: Option<f64>, load: impl Fn(usize) -> f64) -> Vec<f64> {
    let mut ctrl = EnvelopeController::new(config(), 0.0);
    let mut battery = 0.0;
    let mut history: VecDeque<f64> = VecDeque::from(vec![load(0); METER_LAG_TICKS]);
    let mut poi_trace = Vec::with_capacity(TICKS);
    for n in 0..TICKS {
        let true_poi = load(n) - battery;
        history.push_back(true_poi);
        let p_poi = history.pop_front().unwrap();
        poi_trace.push(true_poi);
        let dt = Duration::from_secs(1);
        ctrl.tick(EnvelopeTick {
            import_limit,
            export_limit: Some(0.0),
            active_power: battery,
            requested_setpoint: REQUESTED_W,
            poi_active_power: Some(p_poi),
            power_min: -POWER_MAX,
            power_max: POWER_MAX,
            dt,
        });
        battery = ctrl.current_output();
    }
    poi_trace
}

#[test]
fn a_lagging_meter_never_pushes_the_poi_into_export() {
    // Act
    let poi = simulate(None, |_| LOAD_W);
    // Assert — zero export means the true POI never goes below 0
    let worst = poi.iter().copied().fold(f64::INFINITY, f64::min);
    assert!(worst >= -1.0, "POI exported {:.0} W", -worst);
}

#[test]
fn discharge_settles_at_the_site_load() {
    // Act
    let poi = simulate(None, |_| LOAD_W);
    // Assert — the battery ends up covering the load, POI within 1 kW of 0
    let last = *poi.last().unwrap();
    assert!(last.abs() < 1_000.0, "POI settled at {last:.0} W");
}

#[test]
fn a_zero_band_envelope_holds_the_poi_at_zero_without_ringing() {
    // Arrange — import and export both 0: the battery must carry the whole
    // load, and backing off one side too hard violates the other.
    // Act
    let poi = simulate(Some(0.0), |_| LOAD_W);
    // Assert — never exports, and settles at 0
    let worst = poi.iter().copied().fold(f64::INFINITY, f64::min);
    assert!(worst >= -1.0, "POI exported {:.0} W", -worst);
    let last = *poi.last().unwrap();
    assert!(last.abs() < 1_000.0, "POI settled at {last:.0} W");
}

#[test]
fn export_after_a_load_drop_clears_within_ten_seconds() {
    // Arrange — settled at 1.12 MW, then the load drops to 600 kW at t=60 s.
    // The meter lag means some export is unavoidable; it must not linger.
    const DROP_AT: usize = 60;
    // Act
    let poi = simulate(None, |n| if n < DROP_AT { LOAD_W } else { 600_000.0 });
    // Assert — back to no export within 10 s of the drop
    let late = poi[DROP_AT + 10..]
        .iter()
        .copied()
        .fold(f64::INFINITY, f64::min);
    assert!(
        late >= -1.0,
        "still exporting {:.0} W 10 s after the drop",
        -late
    );
}

#[test]
fn a_zero_band_envelope_rides_load_steps_without_ringing() {
    // Arrange — both limits 0, load drops then comes back. Backing off one
    // side's violation too hard lands straight in the other's, so here the
    // back-off runs at the slow approach gain: no ringing, but ~30 s to settle.
    // Act
    let poi = simulate(Some(0.0), |n| match n {
        0..60 => LOAD_W,
        60..100 => 600_000.0,
        _ => LOAD_W,
    });
    // Assert — after each step the POI never crosses to the other side, and
    // is within 5% of the 520 kW step 30 s later
    for (step, sign) in [(60, -1.0), (100, 1.0)] {
        let after = &poi[step..step + 40];
        assert!(
            after.iter().all(|p| p * sign >= -1.0),
            "rang after step at {step}: {after:?}"
        );
        assert!(
            after[30].abs() < 26_000.0,
            "{:.0} W off 30 s after step at {step}",
            after[30]
        );
    }
}

/// One tick at zero export with a POI meter, 4 MW rating.
fn poi_tick(active_power: f64, requested: f64, p_poi: f64) -> EnvelopeTick {
    EnvelopeTick {
        import_limit: Some(5_378_000.0),
        export_limit: Some(0.0),
        active_power,
        requested_setpoint: requested,
        poi_active_power: Some(p_poi),
        power_min: -POWER_MAX,
        power_max: POWER_MAX,
        dt: Duration::from_secs(1),
    }
}

#[test]
fn a_stale_battery_reading_does_not_raise_the_ceiling() {
    // Arrange — a reserve-floor handoff: the controller holds 1.107 MW,
    // but the module's summed reading still counts a rack that just left
    // (1.65 MW). The POI, 0.4 s behind, already shows 12.8 kW of import.
    let mut ctrl = EnvelopeController::new(config(), 1_107_000.0);
    // Act — the event asks for more than the load
    let out = ctrl.tick(poi_tick(1_650_000.0, REQUESTED_W, 12_800.0));
    // Assert — steps from what was commanded, not the stale reading
    assert_eq!(out, Some(1_107_000.0 + 0.1 * 12_800.0));
}

#[test]
fn a_direct_operator_write_inside_the_envelope_is_left_alone() {
    // Arrange — controller seeded at 0 kW; then the operator's 50 kW goes
    // straight to the racks (handle_command writes directly). 72.8 kW load.
    let mut ctrl = EnvelopeController::new(config(), 0.0);
    ctrl.tick(poi_tick(0.0, 0.0, 72_800.0));
    // Act — next tick sees the new request and the racks at 50 kW
    let out = ctrl.tick(poi_tick(50_000.0, 50_000.0, 22_800.0));
    // Assert — not pulled back toward the controller's own 0
    assert_eq!(out, None);
    assert_eq!(ctrl.current_output(), 50_000.0);
}
