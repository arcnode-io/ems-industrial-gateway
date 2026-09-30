//! POI gate. High-risk: a servo that integrates the same reading twice, or
//! reads a handoff's deliberate under-delivery as headroom, raises past the
//! load into export.

use super::poi_gate::PoiGate;
use std::time::{Duration, Instant};

fn at(t0: Instant, ms: u64) -> Instant {
    t0 + Duration::from_millis(ms)
}

#[test]
fn a_reading_is_fresh_once() {
    // Arrange
    let t0 = Instant::now();
    let mut gate = PoiGate::new();
    // Act
    let first = gate.take(at(t0, 0));
    let again = gate.take(at(t0, 0));
    let next = gate.take(at(t0, 1000));
    // Assert
    assert_eq!(first, (true, false));
    assert_eq!(again, (false, false));
    assert_eq!(next, (true, false));
}

#[test]
fn a_handoff_holds_until_the_second_reading_after_the_growth_write() {
    // Arrange
    let t0 = Instant::now();
    let mut gate = PoiGate::new();
    gate.take(at(t0, 0));
    // Act + Assert — tick 1 defers growth; tick 2 writes it at 1.1 s
    gate.growth_deferred();
    assert_eq!(
        gate.take(at(t0, 1000)),
        (true, true),
        "under-delivery reading"
    );
    gate.growth_written(at(t0, 1100));
    assert_eq!(
        gate.take(at(t0, 1500)),
        (true, true),
        "arrived after, may predate"
    );
    assert_eq!(
        gate.take(at(t0, 1500)),
        (false, true),
        "reused, not counted"
    );
    assert_eq!(
        gate.take(at(t0, 2500)),
        (true, false),
        "second fresh reading lifts it"
    );
}

#[test]
fn a_reading_received_before_the_growth_write_does_not_count() {
    let t0 = Instant::now();
    let mut gate = PoiGate::new();
    gate.growth_deferred();
    gate.growth_written(at(t0, 1100));
    assert_eq!(gate.take(at(t0, 1000)), (true, true));
    assert_eq!(gate.take(at(t0, 1500)), (true, true));
    assert_eq!(gate.take(at(t0, 2500)), (true, false));
}
