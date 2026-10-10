//! A locked-out rack is never written, not even 0 W: the person working on
//! it owns it. The racks still in service take its share.

use super::compute_shares;
use crate::asyncapi::types::DistributeBinding;
use crate::synthetic::{InputCache, new_input_cache};
use serde_json::json;
use std::collections::HashSet;
use std::time::Instant;

fn rack(n: u32) -> serde_json::Value {
    json!({
        "device_id": format!("rack_{n}"),
        "operating_state_topic": format!("sites/{{site_id}}/devices/rack_{n}/state"),
        "state_of_charge_topic": format!("sites/{{site_id}}/devices/rack_{n}/soc"),
        "power_min": -1_000_000.0, "power_max": 1_000_000.0,
    })
}

fn module() -> DistributeBinding {
    serde_json::from_value(json!({
        "allocation_policy": "equal_split",
        "children": [rack(1), rack(2)],
    }))
    .unwrap()
}

fn reporting(cache: &InputCache, n: u32) {
    let now = Instant::now();
    cache.insert(
        format!("sites/s/devices/rack_{n}/state"),
        (json!("STANDBY"), now),
    );
    cache.insert(format!("sites/s/devices/rack_{n}/soc"), (json!(60.0), now));
}

fn locked(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_locked_rack_gets_no_write_and_the_other_takes_its_share() {
    // Arrange
    let cache = new_input_cache();
    reporting(&cache, 1);
    reporting(&cache, 2);
    // Act
    let shares = compute_shares(&module(), 800_000.0, "s", &cache, &locked(&["rack_1"])).unwrap();
    // Assert
    assert_eq!(shares, vec![("rack_2".to_string(), 800_000.0)]);
}

#[test]
fn a_locked_rack_that_never_reported_doesnt_hold_the_module() {
    // Arrange — rack_1 is out for work and silent
    let cache = new_input_cache();
    reporting(&cache, 2);
    // Act
    let shares = compute_shares(&module(), 800_000.0, "s", &cache, &locked(&["rack_1"])).unwrap();
    // Assert
    assert_eq!(shares, vec![("rack_2".to_string(), 800_000.0)]);
}

#[test]
fn with_every_rack_locked_nothing_is_written() {
    let cache = new_input_cache();
    reporting(&cache, 1);
    reporting(&cache, 2);
    let shares = compute_shares(
        &module(),
        800_000.0,
        "s",
        &cache,
        &locked(&["rack_1", "rack_2"]),
    );
    assert!(shares.unwrap().is_empty());
}
