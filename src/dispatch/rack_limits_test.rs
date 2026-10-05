//! Per-rack headroom: static bound capped by the rack's live limit.

use super::{STALE_AFTER, headroom};
use crate::asyncapi::types::ChildAllocation;
use crate::synthetic::{InputCache, new_input_cache};
use serde_json::json;
use std::time::Instant;

const DISCHARGE: &str = "sites/s/devices/rack_1/measurements/max_discharge_power/watts";
const CHARGE: &str = "sites/s/devices/rack_1/measurements/max_charge_power/watts";

fn rack() -> ChildAllocation {
    ChildAllocation {
        device_id: "rack_1".to_string(),
        operating_state_topic: String::new(),
        state_of_charge_topic: String::new(),
        power_min: -4_000_000.0,
        power_max: 4_000_000.0,
    }
}

fn reading(cache: &InputCache, topic: &str, watts: f64, at: Instant) {
    cache.insert(topic.to_string(), (json!(watts), at));
}

#[test]
fn the_live_limit_caps_the_static_bound() {
    // Arrange — 10% SoC: discharge derated to 2 MW, charge at rated
    let cache = new_input_cache();
    reading(&cache, DISCHARGE, 2_000_000.0, Instant::now());
    reading(&cache, CHARGE, 4_000_000.0, Instant::now());
    // Act + Assert
    assert_eq!(headroom(&rack(), 1.0, "s", &cache), 2_000_000.0);
    assert_eq!(headroom(&rack(), -1.0, "s", &cache), 4_000_000.0);
}

#[test]
fn a_limit_above_the_static_bound_does_not_raise_it() {
    let cache = new_input_cache();
    reading(&cache, DISCHARGE, 9_000_000.0, Instant::now());
    assert_eq!(headroom(&rack(), 1.0, "s", &cache), 4_000_000.0);
}

#[test]
fn a_missing_or_stale_limit_falls_back_to_the_static_bound() {
    // Arrange — no charge reading; a discharge reading from before the cutoff
    let cache = new_input_cache();
    let old = Instant::now() - STALE_AFTER - STALE_AFTER;
    reading(&cache, DISCHARGE, 0.0, old);
    // Act + Assert
    assert_eq!(headroom(&rack(), 1.0, "s", &cache), 4_000_000.0);
    assert_eq!(headroom(&rack(), -1.0, "s", &cache), 4_000_000.0);
}
