//! Unit tests for the pure sum helper. High-risk: silently reporting a
//! partial/wrong site total (e.g. one module's active_power stale) would
//! feed a wrong number straight into der-control-api's shortfall detection.

use super::sum_cached;
use crate::synthetic::new_input_cache;
use std::time::Instant;

#[test]
fn sums_every_cached_topic() {
    let cache = new_input_cache();
    cache.insert("a".to_string(), (100.0, Instant::now()));
    cache.insert("b".to_string(), (250.0, Instant::now()));
    let total = sum_cached(&["a".to_string(), "b".to_string()], &cache);
    assert_eq!(total, Some(350.0));
}

#[test]
fn holds_when_any_source_missing() {
    let cache = new_input_cache();
    cache.insert("a".to_string(), (100.0, Instant::now()));
    let total = sum_cached(&["a".to_string(), "b".to_string()], &cache);
    assert_eq!(total, None);
}

#[test]
fn holds_when_no_source_topics_at_all() {
    let cache = new_input_cache();
    let total = sum_cached(&[], &cache);
    assert_eq!(total, None);
}
