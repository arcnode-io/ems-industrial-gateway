//! Unit tests for the max-min fair allocation algorithm.

use super::allocation::{AllocationPolicy, ChildCapacity, OperatingState, allocate};
use std::collections::HashMap;

fn child(device_id: &str, headroom: f64, soc: f64) -> ChildCapacity {
    ChildCapacity {
        device_id: device_id.to_string(),
        operating_state: OperatingState::Standby,
        headroom,
        state_of_charge: soc,
    }
}

#[test]
fn parse_recognizes_both_policies() {
    assert_eq!(
        AllocationPolicy::parse("equal_split").unwrap(),
        AllocationPolicy::EqualSplit
    );
    assert_eq!(
        AllocationPolicy::parse("soc_weighted").unwrap(),
        AllocationPolicy::SocWeighted
    );
}

#[test]
fn parse_rejects_unknown_policy() {
    assert!(AllocationPolicy::parse("first_fit").is_err());
}

#[test]
fn equal_split_divides_evenly_when_no_child_clamps() {
    // Arrange — 3 racks, generous headroom, 300kW to discharge
    let children = [
        child("r1", 500.0, 50.0),
        child("r2", 500.0, 50.0),
        child("r3", 500.0, 50.0),
    ];
    // Act
    let shares = allocate(300.0, &children, AllocationPolicy::EqualSplit);
    // Assert
    for (_, s) in &shares {
        assert!((s - 100.0).abs() < f64::EPSILON);
    }
}

#[test]
fn equal_split_redistributes_clamped_remainder() {
    // Arrange — r1 can only take 20 of an even 100 share; r2/r3 pick up the slack
    let children = [
        child("r1", 20.0, 50.0),
        child("r2", 500.0, 50.0),
        child("r3", 500.0, 50.0),
    ];
    // Act — 300 requested: r1 clamps at 20, remaining 280 split evenly r2/r3
    let shares = allocate(300.0, &children, AllocationPolicy::EqualSplit);
    let by_id: HashMap<_, _> = shares.into_iter().collect();
    // Assert
    assert!((by_id["r1"] - 20.0).abs() < f64::EPSILON);
    assert!((by_id["r2"] - 140.0).abs() < f64::EPSILON);
    assert!((by_id["r3"] - 140.0).abs() < f64::EPSILON);
}

#[test]
fn soc_weighted_favors_higher_soc_on_discharge() {
    // Arrange — r1 at 80% SoC, r2 at 20% SoC, generous headroom, discharging 100kW
    let children = [child("r1", 500.0, 80.0), child("r2", 500.0, 20.0)];
    // Act
    let shares = allocate(100.0, &children, AllocationPolicy::SocWeighted);
    let by_id: HashMap<_, _> = shares.into_iter().collect();
    // Assert — 80/(80+20)*100 = 80, 20/(80+20)*100 = 20
    assert!((by_id["r1"] - 80.0).abs() < f64::EPSILON);
    assert!((by_id["r2"] - 20.0).abs() < f64::EPSILON);
}

#[test]
fn soc_weighted_favors_lower_soc_on_charge() {
    // Arrange — r1 at 80% SoC, r2 at 20% SoC, charging -100kW
    let children = [child("r1", 500.0, 80.0), child("r2", 500.0, 20.0)];
    // Act
    let shares = allocate(-100.0, &children, AllocationPolicy::SocWeighted);
    let by_id: HashMap<_, _> = shares.into_iter().collect();
    // Assert — headroom-to-full weights: r1=(100-80)=20, r2=(100-20)=80
    assert!((by_id["r1"] - -20.0).abs() < f64::EPSILON);
    assert!((by_id["r2"] - -80.0).abs() < f64::EPSILON);
}

#[test]
fn soc_weighted_falls_back_to_equal_split_when_denominator_zero() {
    // Arrange — every eligible rack at 100% SoC, charging (weight = 100-100 = 0 each)
    let children = [child("r1", 500.0, 100.0), child("r2", 500.0, 100.0)];
    // Act
    let shares = allocate(-100.0, &children, AllocationPolicy::SocWeighted);
    let by_id: HashMap<_, _> = shares.into_iter().collect();
    // Assert — falls back to equal split rather than NaN
    assert!((by_id["r1"] - -50.0).abs() < f64::EPSILON);
    assert!((by_id["r2"] - -50.0).abs() < f64::EPSILON);
}

#[test]
fn fault_and_offline_children_are_skipped() {
    // Arrange
    let mut faulted = child("r2", 500.0, 50.0);
    faulted.operating_state = OperatingState::Fault;
    let mut offline = child("r3", 500.0, 50.0);
    offline.operating_state = OperatingState::Offline;
    let children = [child("r1", 500.0, 50.0), faulted, offline];
    // Act
    let shares = allocate(100.0, &children, AllocationPolicy::EqualSplit);
    // Assert — only the healthy rack gets a share
    assert_eq!(shares.len(), 1);
    assert_eq!(shares[0].0, "r1");
    assert!((shares[0].1 - 100.0).abs() < f64::EPSILON);
}

#[test]
fn returns_empty_when_no_eligible_children() {
    let mut faulted = child("r1", 500.0, 50.0);
    faulted.operating_state = OperatingState::Fault;
    let shares = allocate(100.0, &[faulted], AllocationPolicy::EqualSplit);
    assert!(shares.is_empty());
}
