//! The effective reserve floor: the supplier's, or the operator's when it's
//! higher, never below the supplier's.

use super::effective_floor_percent;
use crate::asyncapi::types::DistributeBinding;
use crate::synthetic::new_input_cache;
use serde_json::{Value, json};
use std::time::Instant;

const RESERVE: &str = "sites/s/devices/der_dispatch/measurements/operator_reserve/watt_hours";

fn binding(extra: Value) -> DistributeBinding {
    let mut b = json!({ "allocation_policy": "equal_split", "children": [] });
    b.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(b).unwrap()
}

/// 25% supplier floor, 8 MWh site, operator reserve on its topic.
fn guarded() -> DistributeBinding {
    binding(json!({
        "state_of_charge_floor_percent": 25.0,
        "operator_reserve_topic": RESERVE.replace("/s/", "/{site_id}/"),
        "site_capacity_wh": 8_000_000.0,
    }))
}

#[test]
fn the_operator_reserve_raises_the_floor_when_it_is_higher() {
    // Arrange — keep 4 MWh of 8: 50%
    let cache = new_input_cache();
    cache.insert(RESERVE.to_string(), (json!(4_000_000.0), Instant::now()));
    // Act + Assert
    assert_eq!(effective_floor_percent(&guarded(), "s", &cache), Some(50.0));
}

#[test]
fn the_supplier_floor_always_holds() {
    // Arrange — operator asks for less than the supplier floor (1 MWh = 12.5%)
    let cache = new_input_cache();
    cache.insert(RESERVE.to_string(), (json!(1_000_000.0), Instant::now()));
    // Act + Assert
    assert_eq!(effective_floor_percent(&guarded(), "s", &cache), Some(25.0));
    // ...and with nothing published, it's the supplier floor alone
    assert_eq!(
        effective_floor_percent(&guarded(), "s", &new_input_cache()),
        Some(25.0)
    );
}

#[test]
fn a_reserve_beyond_capacity_means_never_discharge() {
    let cache = new_input_cache();
    cache.insert(RESERVE.to_string(), (json!(20_000_000.0), Instant::now()));
    assert_eq!(
        effective_floor_percent(&guarded(), "s", &cache),
        Some(100.0)
    );
}

#[test]
fn without_capacity_there_is_no_operator_reserve() {
    // device-api omits both fields when capacity is unknown
    let cache = new_input_cache();
    cache.insert(RESERVE.to_string(), (json!(4_000_000.0), Instant::now()));
    let supplier_only = binding(json!({ "state_of_charge_floor_percent": 25.0 }));
    assert_eq!(
        effective_floor_percent(&supplier_only, "s", &cache),
        Some(25.0)
    );
    assert_eq!(
        effective_floor_percent(&binding(json!({})), "s", &cache),
        None
    );
}
