//! Storage spare: requested discharge the racks can deliver, minus output.

use super::{module_spare_w, spare_w};
use crate::asyncapi::types::DistributeBinding;
use crate::synthetic::{InputCache, new_input_cache};
use serde_json::{Value, json};
use std::time::Instant;

const ACTIVE: &str = "sites/s/devices/bess/measurements/active_power/watts";

fn rack(n: u32) -> Value {
    json!({
        "device_id": format!("rack_{n}"),
        "operating_state_topic": format!("sites/{{site_id}}/devices/rack_{n}/state"),
        "state_of_charge_topic": format!("sites/{{site_id}}/devices/rack_{n}/soc"),
        "power_min": -500_000.0,
        "power_max": 500_000.0,
    })
}

const POI: &str = "sites/s/devices/poi/measurements/active_power/watts";

/// Two 500 kW racks, 20% supplier floor, servoing on `POI`.
fn module() -> DistributeBinding {
    serde_json::from_value(json!({
        "allocation_policy": "equal_split",
        "children": [rack(1), rack(2)],
        "state_of_charge_floor_percent": 20.0,
        "power_max": 1_000_000.0,
        "active_power_topic": ACTIVE.replace("/s/", "/{site_id}/"),
        "poi_active_power_topic": POI.replace("/s/", "/{site_id}/"),
    }))
    .unwrap()
}

fn cache(soc: [f64; 2], active_w: f64) -> InputCache {
    let cache = new_input_cache();
    let now = Instant::now();
    for (i, soc) in soc.iter().enumerate() {
        let n = i + 1;
        cache.insert(
            format!("sites/s/devices/rack_{n}/state"),
            (json!("DISCHARGING"), now),
        );
        cache.insert(format!("sites/s/devices/rack_{n}/soc"), (json!(soc), now));
    }
    cache.insert(ACTIVE.to_string(), (json!(active_w), now));
    cache
}

#[test]
fn spare_is_the_rated_discharge_the_racks_can_add() {
    // Arrange — 1 MW rated, delivering 300 kW, both racks above floor
    let cache = cache([60.0, 60.0], 300_000.0);
    // Act
    let spare = module_spare_w(&module(), "s", &cache, &Default::default());
    // Assert
    assert_eq!(spare, 700_000.0);
}

#[test]
fn a_rack_at_its_floor_adds_nothing() {
    // Arrange — rack 2 at the floor: only rack 1's 500 kW is deliverable
    let cache = cache([60.0, 20.0], 300_000.0);
    // Act
    let spare = module_spare_w(&module(), "s", &cache, &Default::default());
    // Assert
    assert_eq!(spare, 200_000.0);
}

#[test]
fn no_spare_without_a_reading() {
    assert_eq!(
        module_spare_w(&module(), "s", &new_input_cache(), &Default::default()),
        0.0
    );
}

#[test]
fn only_batteries_on_the_same_poi_count() {
    // Arrange
    let cache = cache([60.0, 60.0], 300_000.0);
    let other = "sites/s/devices/other_poi/measurements/active_power/watts";
    // Act + Assert
    assert_eq!(
        spare_w(&[module()], POI, "s", &cache, &Default::default()),
        700_000.0
    );
    assert_eq!(
        spare_w(&[module()], other, "s", &cache, &Default::default()),
        0.0
    );
}
