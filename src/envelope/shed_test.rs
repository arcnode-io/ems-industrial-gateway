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
        storage_spare_w: 0.0,
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

#[test]
fn import_still_falling_means_storage_is_still_covering_so_no_shed() {
    // Arrange — battery ramping in: import over the limit but shrinking
    // 1 kW a tick, for longer than the dwell, down to 20 kW
    let mut c = controller();
    let ramping: Vec<Option<f64>> = (0..=40)
        .map(|n| {
            let p = c.tick(&tick(60_000.0 - 1_000.0 * f64::from(n)));
            if let Some(p) = p {
                c.confirm(p);
            }
            p
        })
        .collect();
    // Assert — nothing shed while storage was still closing the gap
    assert!(ramping.iter().all(Option::is_none), "{ramping:?}");
    // Act — storage stops (floor): import holds flat; the dwell starts now
    assert_eq!(run(&mut c, tick(20_000.0), 29), None);
    assert!(c.tick(&tick(20_000.0)).is_some());
}

#[test]
fn a_small_gap_is_cut_by_no_more_than_the_gap() {
    // Arrange — storage power-limited 830 W short (0.83% of a 100 kW fleet)
    let mut c = controller();
    assert_eq!(run(&mut c, tick(830.0), 29), None);
    // Act
    let cut = c.tick(&tick(830.0));
    // Assert — 0.8%, not a whole 1% that would cut 1 kW and export 170 W
    assert_eq!(cut, Some(99.2));
}

/// Shed to the 20% floor by a 500 kW gap storage couldn't cover.
fn shed_to_floor() -> ShedController {
    let mut c = controller();
    run(&mut c, tick(500_000.0), 60);
    c
}

#[test]
fn spare_storage_takes_compute_back_while_the_limit_is_met() {
    // Arrange — operator released the battery mid-event: the limit is met
    // exactly (POI 0 against 0) and storage has 60 kW it isn't using
    let mut c = shed_to_floor();
    let met = ShedTick {
        storage_spare_w: 60_000.0,
        ..tick(0.0)
    };
    // Act + Assert — after the dwell, caps rise by no more than the battery
    // can follow through the meter's lag: margin (5 kW) over 3 s per tick
    assert_eq!(run(&mut c, met, 29), None);
    let first = c.tick(&met).expect("hands back");
    assert!((20.0..=21.7).contains(&first), "{first}");
}

#[test]
fn without_spare_storage_compute_stays_shed_while_the_limit_is_met() {
    let mut c = shed_to_floor();
    assert_eq!(run(&mut c, tick(0.0), 60), None);
}
