//! Cease to energize: only an explicit false stops the module.

use super::target_w;
use crate::synthetic::new_input_cache;
use serde_json::json;
use std::time::Instant;

const ENERGIZE: &str = "sites/s/devices/der_dispatch/measurements/energize_enabled/none";

#[test]
fn energize_false_holds_the_module_at_zero() {
    let cache = new_input_cache();
    cache.insert(ENERGIZE.to_string(), (json!(false), Instant::now()));
    assert_eq!(target_w(500_000.0, "s", &cache), 0.0);
    assert_eq!(target_w(-248_200.0, "s", &cache), 0.0);
}

#[test]
fn energize_true_or_never_published_leaves_the_target_alone() {
    // never published: every site that hasn't received an energize control
    assert_eq!(target_w(500_000.0, "s", &new_input_cache()), 500_000.0);
    let cache = new_input_cache();
    cache.insert(ENERGIZE.to_string(), (json!(true), Instant::now()));
    assert_eq!(target_w(500_000.0, "s", &cache), 500_000.0);
}
