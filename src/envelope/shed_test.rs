//! Compute shed: the fleet cap percentage the envelope drives when storage
//! can't keep the POI inside its limits.

use super::{ShedController, ShedTick};
use crate::envelope::inputs::EnvelopeConfig;
use std::time::Duration;

/// 100 GPUs at 1000 W max (100 kW fleet), 200 W floor; 30 s dwell, 10%/s ramp.
fn controller() -> ShedController {
    ShedController::new(EnvelopeConfig {
        ramp_rate_per_sec: 0.10,
        hysteresis_margin: 0.05,
        hysteresis_dwell: Duration::from_secs(30),
    })
}

fn tick(poi_w: f64) -> ShedTick {
    ShedTick {
        poi_active_power: poi_w,
        import_limit: 0.0,
        export_limit: 0.0,
        requested_percent: 100.0,
        fleet_max_w: 100_000.0,
        fleet_min_w: 20_000.0,
        dt: Duration::from_secs(1),
    }
}

/// Feed the same tick `secs` times, confirming every proposal as a
/// successful write would; the last output.
fn run(c: &mut ShedController, t: ShedTick, secs: u32) -> Option<f64> {
    (0..secs).fold(None, |last, _| {
        let proposed = c.tick(&t);
        if let Some(p) = proposed {
            c.confirm(p);
        }
        proposed.or(last)
    })
}

#[test]
fn import_over_the_limit_waits_out_the_dwell_before_shedding() {
    // Arrange — 30 kW over a zero import limit
    let mut c = controller();
    // Act + Assert — the battery gets the dwell to cover it first
    assert_eq!(run(&mut c, tick(30_000.0), 29), None);
    // Past the dwell: cut by the violation, ramp-limited (10%/s), whole %
    assert_eq!(c.tick(&tick(30_000.0)), Some(90.0));
}

#[test]
fn shedding_stops_at_the_gpu_floor() {
    let mut c = controller();
    // 500 kW over: far more than compute can give
    let last = run(&mut c, tick(500_000.0), 60);
    assert_eq!(last, Some(20.0));
}

#[test]
fn headroom_after_a_shed_restores_toward_the_requested_cap() {
    // Arrange — shed to 80%
    let mut c = controller();
    run(&mut c, tick(30_000.0), 31);
    // Act — envelope lifts: plenty of import room
    let mut lifted = tick(-0.0);
    lifted.import_limit = 1_000_000.0;
    let restored = run(&mut c, lifted, 40);
    // Assert — back to the operator's cap, never above
    assert_eq!(restored, Some(100.0));
    assert_eq!(c.tick(&lifted), None);
}

#[test]
fn exporting_past_the_limit_restores_caps_without_waiting() {
    // Arrange — shed to 80%, then the battery overshoots into export
    let mut c = controller();
    run(&mut c, tick(30_000.0), 31);
    // Act — 10 kW export against a zero export limit
    let raised = c.tick(&tick(-10_000.0));
    // Assert — more load at once: shedding is now the problem
    assert!(raised.is_some_and(|p| p > 80.0), "{raised:?}");
}

#[test]
fn nothing_moves_inside_the_envelope() {
    let mut c = controller();
    let mut inside = tick(500_000.0);
    inside.import_limit = 1_000_000.0;
    assert_eq!(run(&mut c, inside, 60), None);
}

#[test]
fn a_cap_that_failed_to_write_is_proposed_again() {
    // Arrange — wait out the dwell, then the first cut is proposed
    let mut c = controller();
    assert_eq!(run(&mut c, tick(30_000.0), 29), None);
    assert_eq!(c.tick(&tick(30_000.0)), Some(90.0));
    // Act — the write failed, so it isn't confirmed
    let again = c.tick(&tick(30_000.0));
    // Assert — the devices are still at 100%: propose the same cut again
    assert_eq!(again, Some(90.0));
    // ...and once confirmed, the next cut builds on it
    c.confirm(90.0);
    assert_eq!(c.tick(&tick(30_000.0)), Some(80.0));
}
