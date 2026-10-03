//! A setpoint written straight to the battery (a dispatch landing) in the
//! same moment the envelope binds. High-risk: the meter hasn't seen the step
//! yet, so its import reads as a violation the new setpoint already covers;
//! integrating it on top exported ~400 kW under a zero-export envelope on
//! the demo.

use super::control_law::{EnvelopeConfig, EnvelopeController, EnvelopeTick};
use std::time::Duration;

const LOAD_W: f64 = 1_120_000.0;
/// What the dispatch commands: a little past the load.
const DISPATCHED_W: f64 = 1_196_500.0;
const POWER_MAX: f64 = 4_000_000.0;

/// Closed loop, meter `lag` ticks late. The envelope closes to zero import
/// at tick 2 and the dispatch lands at tick 3. Returns the true POI each
/// tick.
fn simulate(lag: usize) -> Vec<f64> {
    simulate_dispatch(lag, DISPATCHED_W)
}

/// `simulate`, with the dispatch landing at `dispatched` W.
fn simulate_dispatch(lag: usize, dispatched: f64) -> Vec<f64> {
    let config = EnvelopeConfig {
        ramp_rate_per_sec: 0.10,
        hysteresis_margin: 0.05,
        hysteresis_dwell: Duration::from_secs(30),
    };
    let mut ctrl = EnvelopeController::new(config, 0.0);
    let mut battery = 0.0;
    let mut meter = std::collections::VecDeque::from(vec![LOAD_W; lag]);
    (0..120)
        .map(|n| {
            let requested = if n >= 3 { dispatched } else { 0.0 };
            let import_limit = if n >= 2 { 0.0 } else { 2_000_000.0 };
            ctrl.tick(EnvelopeTick {
                import_limit: Some(import_limit),
                export_limit: Some(0.0),
                active_power: battery,
                requested_setpoint: requested,
                poi_active_power: Some(*meter.front().unwrap()),
                poi_fresh: true,
                hold_approach: false,
                power_min: -POWER_MAX,
                power_max: POWER_MAX,
                dt: Duration::from_secs(1),
            });
            battery = ctrl.current_output();
            let true_poi = LOAD_W - battery;
            meter.pop_front();
            meter.push_back(true_poi);
            true_poi
        })
        .collect()
}

#[test]
fn a_dispatch_landing_with_the_envelope_doesnt_overshoot_into_export() {
    // The meter lags 1–3 s, the range the POI servo is tuned for
    for lag in 1..=3 {
        // Act
        let poi = simulate(lag);
        // Assert — no deeper export than the dispatch itself asked for
        // (76.5 kW, which the servo then backs off), never the stale import
        // added on top
        let worst = poi.iter().copied().fold(f64::INFINITY, f64::min);
        assert!(
            worst >= LOAD_W - DISPATCHED_W - 1.0,
            "lag {lag}: exported {:.0} W",
            -worst
        );
    }
}

#[test]
fn an_envelope_only_event_brings_import_to_zero_without_exporting() {
    // A line-constraint event dispatches nothing (target 0): the envelope
    // alone drives the battery, from a 1.12 MW violation
    for lag in 1..=3 {
        let poi = simulate_dispatch(lag, 0.0);
        let worst = poi.iter().copied().fold(f64::INFINITY, f64::min);
        assert!(worst >= -1.0, "lag {lag}: exported {:.0} W", -worst);
        assert!(
            poi.last().unwrap().abs() < 1_000.0,
            "lag {lag}: settled at {:.0} W",
            poi.last().unwrap()
        );
    }
}
