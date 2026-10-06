//! Closed-loop onset when the import limit drops. High-risk both ways: a
//! slow onset leaves the site importing past its limit for tens of seconds;
//! acting twice on one jump (once from the limit change, again from the
//! lagged meter still showing it) overshoots into export, a reverse-power
//! trip on real hardware.

use super::control_law::{EnvelopeConfig, EnvelopeController, EnvelopeTick};
use std::collections::VecDeque;
use std::time::Duration;

const LOAD_W: f64 = 1_120_000.0;
const RECHARGE_W: f64 = -248_000.0;
const METER_LAG_TICKS: usize = 2;
/// The import limit drops from 4.78 MW to 0 at this tick.
const TIGHTEN_AT: usize = 10;

fn config() -> EnvelopeConfig {
    EnvelopeConfig {
        ramp_rate_per_sec: 0.10,
        hysteresis_margin: 0.05,
        hysteresis_dwell: Duration::from_secs(30),
    }
}

/// True POI (+ import) each tick. Recharging until the limit drops (the
/// event also ends the recharge request); `load(n)` is the site load.
fn simulate(load: impl Fn(usize) -> f64) -> (Vec<f64>, Vec<f64>) {
    let mut ctrl = EnvelopeController::new(config(), RECHARGE_W);
    let mut battery = RECHARGE_W;
    let mut meter: VecDeque<f64> = VecDeque::from(vec![load(0) - battery; METER_LAG_TICKS]);
    let (mut batteries, mut pois) = (Vec::new(), Vec::new());
    for n in 0..90 {
        meter.push_back(load(n) - battery);
        let p_poi = meter.pop_front().unwrap();
        let (import_limit, requested) = if n < TIGHTEN_AT {
            (4_780_000.0, RECHARGE_W)
        } else {
            (0.0, 0.0)
        };
        ctrl.tick(EnvelopeTick {
            import_limit: Some(import_limit),
            export_limit: Some(0.0),
            active_power: battery,
            requested_setpoint: requested,
            poi_active_power: Some(p_poi),
            poi_fresh: true,
            hold_approach: false,
            power_min: -3_854_000.0,
            power_max: 3_854_000.0,
            dt: Duration::from_secs(1),
        });
        battery = ctrl.current_output();
        batteries.push(battery);
        pois.push(load(n) - battery);
    }
    (batteries, pois)
}

#[test]
fn a_dropped_import_limit_is_covered_on_the_next_tick() {
    // Act
    let (batteries, _) = simulate(|_| LOAD_W);
    // Assert — the whole load, recharge included, in one step
    let first = batteries[TIGHTEN_AT];
    assert!((first - LOAD_W).abs() < 1.0, "first step to {first:.0} W");
}

#[test]
fn the_onset_never_exports_with_a_steady_load() {
    // Act
    let (_, pois) = simulate(|_| LOAD_W);
    // Assert — the lagged meter still showing the old violation isn't acted on again
    let worst = pois.iter().copied().fold(f64::INFINITY, f64::min);
    assert!(worst >= -1.0, "POI exported {:.0} W", -worst);
}

#[test]
fn a_load_drop_inside_the_meter_lag_exports_only_that_drop_then_settles() {
    // Arrange — the load falls 300 kW one tick after the limit drops
    let load = |n: usize| {
        if n > TIGHTEN_AT {
            LOAD_W - 300_000.0
        } else {
            LOAD_W
        }
    };
    // Act
    let (_, pois) = simulate(load);
    // Assert — no more than the drop itself, and within 5% of it 30 s on:
    // with both limits at 0 the back-off runs at the slow approach gain so
    // it can't ring into import (see `a_zero_band_envelope_rides_load_steps`)
    let worst = pois.iter().copied().fold(f64::INFINITY, f64::min);
    assert!(worst >= -300_001.0, "POI exported {:.0} W", -worst);
    let settled = pois[TIGHTEN_AT + 31];
    assert!(
        settled >= -15_000.0,
        "still exporting {settled:.0} W 30 s on"
    );
}
