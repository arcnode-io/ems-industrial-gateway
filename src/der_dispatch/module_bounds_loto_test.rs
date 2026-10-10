//! Site distribution splits only across what it may write: a locked-out
//! module gets no share, and a module's locked racks add no headroom.

use super::modules_with_bounds;
use crate::asyncapi::types::ProtocolBinding;
use crate::synthetic::{InputCache, new_input_cache};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// A module of two 1 MW racks, with its SoC cached.
fn module(cache: &InputCache, id: &str) -> (String, HashMap<String, ProtocolBinding>) {
    cache.insert(
        format!("sites/s/devices/{id}/measurements/state_of_charge/percent"),
        (json!(60.0), Instant::now()),
    );
    let rack = |n: u32| {
        json!({
            "device_id": format!("{id}_rack_{n}"),
            "operating_state_topic": "unused",
            "state_of_charge_topic": "unused",
            "power_min": -1_000_000.0, "power_max": 1_000_000.0,
        })
    };
    let binding: ProtocolBinding = serde_json::from_value(json!({
        "protocol": "distribute", "allocation_policy": "equal_split",
        "power_min": -2_000_000.0, "power_max": 2_000_000.0,
        "children": [rack(1), rack(2)],
    }))
    .unwrap();
    (
        id.to_string(),
        HashMap::from([("set_active_power".to_string(), binding)]),
    )
}

fn locked(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_locked_module_gets_no_share() {
    // Arrange
    let cache = new_input_cache();
    let channels = HashMap::from([module(&cache, "m1"), module(&cache, "m2")]);
    // Act
    let modules = modules_with_bounds(&channels, &cache, "s", 1.0, &locked(&["m1"])).unwrap();
    // Assert
    let ids: Vec<&str> = modules.iter().map(|m| m.device_id.as_str()).collect();
    assert_eq!(ids, vec!["m2"]);
}

#[test]
fn a_locked_module_never_heard_from_doesnt_hold_the_site() {
    // Arrange — m1 is out for work and silent
    let cache = new_input_cache();
    let channels = HashMap::from([module(&new_input_cache(), "m1"), module(&cache, "m2")]);
    // Act
    let modules = modules_with_bounds(&channels, &cache, "s", 1.0, &locked(&["m1"]));
    // Assert
    assert_eq!(modules.map(|m| m.len()), Some(1));
}

#[test]
fn a_locked_rack_adds_no_headroom_to_its_module() {
    // Arrange
    let cache = new_input_cache();
    let channels = HashMap::from([module(&cache, "m1")]);
    // Act
    let modules =
        modules_with_bounds(&channels, &cache, "s", 1.0, &locked(&["m1_rack_1"])).unwrap();
    // Assert
    assert_eq!(modules[0].headroom, 1_000_000.0);
}
