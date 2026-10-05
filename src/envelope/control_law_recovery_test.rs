//! Closed-loop recovery once a constraint lifts. High-risk: an envelope that
//! holds the battery covering site load after the import limit widens drains
//! the pack to its floor with nothing asking for discharge (seen on the
//! demo: 1.11 MW held for minutes at POI 8 kW against a 4.3 MW limit).

use super::control_law::{EnvelopeConfig, EnvelopeController, EnvelopeTick};
use std::collections::VecDeque;
use std::time::Duration;

const LOAD_W: f64 = 1_120_000.0;
const POWER_MAX: f64 = 3_854_000.0;
const METER_LAG_TICKS: usize = 2;
/// The import limit widens at this tick.
const LIFT_AT: usize = 120;

fn config() -> EnvelopeConfig {
    EnvelopeConfig {
        ramp_rate_per_sec: 0.10,
        hysteresis_margin: 0.05,
        hysteresis_dwell: Duration::from_secs(30),
    }
}

/// Battery output each tick: nothing requested, zero export, an import
/// limit of 168 kW that widens to 4.3 MW at `LIFT_AT`.
fn simulate(ticks: usize) -> Vec<f64> {
    let mut ctrl = EnvelopeController::new(config(), 0.0);
    let mut battery = 0.0;
    let mut history: VecDeque<f64> = VecDeque::from(vec![LOAD_W; METER_LAG_TICKS]);
    let mut trace = Vec::with_capacity(ticks);
    for n in 0..ticks {
        history.push_back(LOAD_W - battery);
        let p_poi = history.pop_front().unwrap();
        let import_limit = if n < LIFT_AT { 168_380.0 } else { 4_300_000.0 };
        ctrl.tick(EnvelopeTick {
            import_limit: Some(import_limit),
            export_limit: Some(0.0),
            active_power: battery,
            requested_setpoint: 0.0,
            poi_active_power: Some(p_poi),
            poi_fresh: true,
            hold_approach: false,
            power_min: -POWER_MAX,
            power_max: POWER_MAX,
            dt: Duration::from_secs(1),
        });
        battery = ctrl.current_output();
        trace.push(battery);
    }
    trace
}

#[test]
fn the_battery_is_released_once_the_import_limit_widens() {
    // Act
    let trace = simulate(LIFT_AT + 60);
    // Assert — covering the load while constrained...
    let constrained = trace[LIFT_AT - 1];
    assert!(constrained > 900_000.0, "{constrained}");
    // ...back to the operator's 0 W within dwell (30 s) plus the ramp
    let released = trace[LIFT_AT + 59];
    assert_eq!(released, 0.0, "still discharging {released} W");
}
