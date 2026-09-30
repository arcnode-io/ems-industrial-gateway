//! Write order when shares move between children. High-risk: writes land one
//! device at a time, so raising one rack before lowering another briefly
//! puts both on the bus at once. On the demo that 1.65 MW blip, at a 1.12 MW
//! load, exported into the POI when a rack hit its reserve floor.

use super::reductions_first;
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
