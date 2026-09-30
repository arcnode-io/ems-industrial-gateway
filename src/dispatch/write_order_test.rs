//! Write order when shares move between children. High-risk: each rack takes
//! its setpoint with its own latency, so raising one rack before another has
//! dropped briefly puts both on the bus at once. At a reserve-floor handoff
//! that exports the leaving rack's whole share into the POI.

use super::write_order::{handoff_batch, reductions_first};
use std::collections::HashMap;

#[test]
fn a_shrinking_share_is_written_before_a_growing_one() {
    // Arrange — rack_2 hits the floor, rack_1 picks up its share
    let last = HashMap::from([
        ("rack_1".to_string(), 560_000.0),
        ("rack_2".to_string(), 546_000.0),
    ]);
    let changed = vec![
        ("rack_1".to_string(), 1_106_000.0),
        ("rack_2".to_string(), 0.0),
    ];
    // Act
    let ordered = reductions_first(changed, &last);
    // Assert
    assert_eq!(ordered[0].0, "rack_2");
    assert_eq!(ordered[1].0, "rack_1");
}

#[test]
fn charging_reductions_also_go_first() {
    // Arrange — magnitude, not sign: moving toward 0 from charging is a reduction
    let last = HashMap::from([
        ("rack_1".to_string(), -500_000.0),
        ("rack_2".to_string(), -500_000.0),
    ]);
    let changed = vec![
        ("rack_1".to_string(), -1_000_000.0),
        ("rack_2".to_string(), 0.0),
    ];
    // Act
    let ordered = reductions_first(changed, &last);
    // Assert
    assert_eq!(ordered[0].0, "rack_2");
}

#[test]
fn a_never_written_child_counts_as_growing_from_zero() {
    // Arrange
    let last = HashMap::from([("rack_1".to_string(), 800_000.0)]);
    let changed = vec![
        ("rack_2".to_string(), 400_000.0),
        ("rack_1".to_string(), 400_000.0),
    ];
    // Act
    let ordered = reductions_first(changed, &last);
    // Assert
    assert_eq!(ordered[0].0, "rack_1");
}

fn shares(pairs: &[(&str, f64)]) -> Vec<(String, f64)> {
    pairs
        .iter()
        .map(|(id, v)| ((*id).to_string(), *v))
        .collect()
}

#[test]
fn a_handoff_writes_the_shrink_now_and_defers_the_growth() {
    // Arrange — rack_2 hits the floor, rack_1 picks up its share
    let last = HashMap::from([
        ("rack_1".to_string(), 560_000.0),
        ("rack_2".to_string(), 546_000.0),
    ]);
    let changed = shares(&[("rack_2", 0.0), ("rack_1", 1_106_000.0)]);
    // Act
    let (batch, deferred) = handoff_batch(changed, &last, false);
    // Assert
    assert_eq!(batch, shares(&[("rack_2", 0.0)]));
    assert!(deferred);
}

#[test]
fn growth_deferred_last_tick_is_written_this_tick() {
    // Arrange — the shrink landed last tick; SoC drift shrinks rack_2 again
    let last = HashMap::from([
        ("rack_1".to_string(), 560_000.0),
        ("rack_2".to_string(), 546_000.0),
    ]);
    let changed = shares(&[("rack_2", 545_800.0), ("rack_1", 560_200.0)]);
    // Act
    let (batch, deferred) = handoff_batch(changed.clone(), &last, true);
    // Assert — never deferred twice running, or drift would starve rack_1
    assert_eq!(batch, changed);
    assert!(!deferred);
}

#[test]
fn growth_with_nothing_shrinking_goes_straight_out() {
    let last = HashMap::from([("rack_1".to_string(), 100_000.0)]);
    let changed = shares(&[("rack_1", 200_000.0)]);
    let (batch, deferred) = handoff_batch(changed.clone(), &last, false);
    assert_eq!(batch, changed);
    assert!(!deferred);
}
