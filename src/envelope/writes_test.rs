//! Envelope write planning: changes in handoff order, and every rack's
//! share re-asserted each tick.

use super::WriteState;

fn shares(r1: f64, r2: f64) -> Vec<(String, f64)> {
    vec![("rack_1".to_string(), r1), ("rack_2".to_string(), r2)]
}

/// A state whose racks have `r1`/`r2` landed.
fn landed(r1: f64, r2: f64) -> WriteState {
    let mut w = WriteState::default();
    let plan = w.plan(shares(r1, r2)).unwrap();
    w.landed(plan);
    w
}

#[test]
fn unchanged_shares_are_written_again() {
    // Arrange — a rack that rebooted holds its own default, not our share
    let mut w = landed(0.0, 0.0);
    // Act
    let plan = w.plan(shares(0.0, 0.0)).expect("re-asserts");
    // Assert
    assert_eq!(plan.writes, shares(0.0, 0.0));
}

#[test]
fn a_held_back_raise_re_asserts_the_last_share() {
    // Arrange — rack_1 hands 400 kW to rack_2
    let mut w = landed(500_000.0, 100_000.0);
    // Act
    let plan = w.plan(shares(100_000.0, 500_000.0)).unwrap();
    // Assert — the drop lands now; the raise waits a tick, its old share kept
    assert_eq!(plan.writes, shares(100_000.0, 100_000.0));
}
