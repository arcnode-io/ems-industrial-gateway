//! Unit tests for synthetic operations.

use super::*;

#[test]
fn parse_recognizes_every_operation() {
    for (name, expected) in [
        ("subtract", Operation::Subtract),
        ("sum", Operation::Sum),
        ("mean", Operation::Mean),
        ("max", Operation::Max),
        ("min", Operation::Min),
        ("unbalance", Operation::Unbalance),
    ] {
        assert_eq!(Operation::parse(name).unwrap(), expected);
    }
}

#[test]
fn parse_rejects_unknown_operation() {
    assert!(Operation::parse("divide").is_err());
}

#[test]
fn subtract_returns_a_minus_b() {
    // Arrange — DOE import_limit (10MW) minus active_power (3MW) = 7MW headroom
    let inputs = [10_000_000.0, 3_000_000.0];
    // Act
    let got = Operation::Subtract.apply(&inputs).unwrap();
    // Assert
    assert!((got - 7_000_000.0).abs() < f64::EPSILON);
}

#[test]
fn subtract_rejects_wrong_arity() {
    assert!(Operation::Subtract.apply(&[1.0]).is_err());
    assert!(Operation::Subtract.apply(&[1.0, 2.0, 3.0]).is_err());
}

#[test]
fn sum_mean_max_min_compute_correctly() {
    let inputs = [1.0, 2.0, 3.0, 4.0];
    assert!((Operation::Sum.apply(&inputs).unwrap() - 10.0).abs() < f64::EPSILON);
    assert!((Operation::Mean.apply(&inputs).unwrap() - 2.5).abs() < f64::EPSILON);
    assert!((Operation::Max.apply(&inputs).unwrap() - 4.0).abs() < f64::EPSILON);
    assert!((Operation::Min.apply(&inputs).unwrap() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn empty_inputs_rejected_for_aggregate_operations() {
    for f in [
        Operation::Sum,
        Operation::Mean,
        Operation::Max,
        Operation::Min,
    ] {
        assert!(f.apply(&[]).is_err());
    }
}

#[test]
fn weighted_mean_weights_by_capacity() {
    // Arrange — two racks: 50% SoC at 2 MWh, 80% SoC at 1 MWh
    let pairs = [(50.0, 2.0), (80.0, 1.0)];
    // Act
    let got = weighted_mean(&pairs).unwrap();
    // Assert — (50*2 + 80*1) / (2+1) = 60.0, not the flat mean of 65.0
    assert!((got - 60.0).abs() < f64::EPSILON);
}

#[test]
fn weighted_mean_with_equal_weights_matches_flat_mean() {
    let pairs = [(10.0, 1.0), (20.0, 1.0)];
    let got = weighted_mean(&pairs).unwrap();
    assert!((got - 15.0).abs() < f64::EPSILON);
}

#[test]
fn weighted_mean_rejects_empty_pairs() {
    assert!(weighted_mean(&[]).is_err());
}

#[test]
fn weighted_mean_rejects_zero_total_weight() {
    assert!(weighted_mean(&[(50.0, 0.0), (80.0, 0.0)]).is_err());
}

#[test]
fn unbalance_is_zero_for_balanced_phases() {
    assert!(
        Operation::Unbalance
            .apply(&[7200.0, 7200.0, 7200.0])
            .unwrap()
            .abs()
            < f64::EPSILON
    );
}

#[test]
fn unbalance_is_the_largest_deviation_over_the_mean_in_percent() {
    // Arrange — one phase sagging 5%
    let inputs = [7200.0, 7200.0, 6840.0];
    // Act
    let got = Operation::Unbalance.apply(&inputs).unwrap();
    // Assert — avg 7080, max deviation 240 → 100 × 240 / 7080
    assert!((got - 3.389_830_508).abs() < 1e-6);
}

#[test]
fn unbalance_rejects_a_dead_bus() {
    // all phases at 0 V — unbalance is undefined, not 0%
    assert!(Operation::Unbalance.apply(&[0.0, 0.0, 0.0]).is_err());
}
