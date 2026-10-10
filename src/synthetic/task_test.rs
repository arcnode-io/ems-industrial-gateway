use super::*;
use crate::synthetic::cache::new_input_cache;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_secs(5);

/// Every topic used below polled, with a 5 s limit.
fn limits() -> HashMap<String, Duration> {
    ["a", "b"].map(|t| (t.to_string(), LIMIT)).into()
}

#[test]
fn gather_inputs_holds_when_any_input_missing() {
    // Arrange — one of two topics not yet cached
    let cache = new_input_cache();
    cache.insert("a".into(), (serde_json::json!(10.0), Instant::now()));
    // Act
    let result = gather_inputs(&["a".into(), "b".into()], &cache, &limits());
    // Assert
    assert!(result.is_none(), "hold when any input missing");
}

#[test]
fn gather_inputs_returns_values_when_all_cached() {
    // Arrange — both inputs cached
    let cache = new_input_cache();
    cache.insert("a".into(), (serde_json::json!(10.0), Instant::now()));
    cache.insert("b".into(), (serde_json::json!(3.0), Instant::now()));
    // Act
    let values = gather_inputs(&["a".into(), "b".into()], &cache, &limits()).unwrap();
    // Assert
    assert_eq!(values, vec![10.0, 3.0]);
}

#[test]
fn gather_pairs_holds_when_any_pair_missing() {
    // Arrange — one of two topics not yet cached
    let cache = new_input_cache();
    cache.insert("a".into(), (serde_json::json!(50.0), Instant::now()));
    // Act
    let result = gather_pairs(&[("a".into(), 2.0), ("b".into(), 1.0)], &cache, &limits());
    // Assert
    assert!(result.is_none(), "hold when any pair missing");
}

#[test]
fn gather_pairs_returns_value_weight_pairs_when_all_cached() {
    // Arrange
    let cache = new_input_cache();
    cache.insert("a".into(), (serde_json::json!(50.0), Instant::now()));
    cache.insert("b".into(), (serde_json::json!(80.0), Instant::now()));
    // Act
    let pairs = gather_pairs(&[("a".into(), 2.0), ("b".into(), 1.0)], &cache, &limits()).unwrap();
    // Assert — weight carried through unchanged, value from the cache
    assert_eq!(pairs, vec![(50.0, 2.0), (80.0, 1.0)]);
}

#[test]
fn hz_to_period_clamps_to_minimum_1ms() {
    assert_eq!(hz_to_period_ms(2000.0), 1);
    assert_eq!(hz_to_period_ms(1.0), 1000);
    assert_eq!(hz_to_period_ms(0.0033), 303_030);
}

#[test]
fn gather_inputs_holds_when_any_input_went_quiet() {
    // Arrange — b's last reading is older than the limit
    let cache = new_input_cache();
    cache.insert("a".into(), (serde_json::json!(10.0), Instant::now()));
    cache.insert(
        "b".into(),
        (serde_json::json!(3.0), Instant::now() - 2 * LIMIT),
    );
    // Act
    let result = gather_inputs(&["a".into(), "b".into()], &cache, &limits());
    // Assert
    assert!(result.is_none(), "a dead input holds the publish");
}

#[test]
fn gather_pairs_holds_when_any_pair_went_quiet() {
    // Arrange
    let cache = new_input_cache();
    cache.insert(
        "a".into(),
        (serde_json::json!(50.0), Instant::now() - 2 * LIMIT),
    );
    // Act
    let result = gather_pairs(&[("a".into(), 1.0)], &cache, &limits());
    // Assert
    assert!(result.is_none(), "a dead pair holds the publish");
}

#[test]
fn an_unpolled_input_holds_its_last_value() {
    // Arrange — an on-change topic, last published long ago
    let cache = new_input_cache();
    cache.insert(
        "limit".into(),
        (serde_json::json!(7.0), Instant::now() - 100 * LIMIT),
    );
    // Act
    let values = gather_inputs(&["limit".into()], &cache, &limits());
    // Assert
    assert_eq!(values, Some(vec![7.0]));
}
