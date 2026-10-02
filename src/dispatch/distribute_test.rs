//! Unit tests for distribute-binding allocation and cache resolution.

use super::*;
use crate::asyncapi::types::ChildAllocation;
use crate::synthetic::new_input_cache;
use serde_json::json;
use std::time::Instant;

#[test]
fn resolve_child_substitutes_site_id_before_cache_lookup() {
    // Arrange — cache keyed by the RESOLVED topic (site_id substituted),
    // matching what app.rs's subscription list actually caches under.
    let cache = new_input_cache();
    cache.insert(
        "sites/local_site/devices/rack_1/measurements/operating_state/none".into(),
        (json!(0.0), Instant::now()),
    );
    cache.insert(
        "sites/local_site/devices/rack_1/measurements/state_of_charge/percent".into(),
        (json!(65.0), Instant::now()),
    );
    let child = ChildAllocation {
        device_id: "rack_1".to_string(),
        operating_state_topic: "sites/{site_id}/devices/rack_1/measurements/operating_state/none"
            .to_string(),
        state_of_charge_topic:
            "sites/{site_id}/devices/rack_1/measurements/state_of_charge/percent".to_string(),
        power_min: -4_000_000.0,
        power_max: 4_000_000.0,
    };
    // Act
    let resolved = resolve_child(&child, 100.0, "local_site", &cache).unwrap();
    // Assert
    assert_eq!(resolved.operating_state, OperatingState::Standby);
    assert!((resolved.state_of_charge - 65.0).abs() < f64::EPSILON);
}

#[test]
fn operating_state_from_f64_maps_all_five_values() {
    for (raw, expected) in [
        (0.0, OperatingState::Standby),
        (1.0, OperatingState::Charging),
        (2.0, OperatingState::Discharging),
        (3.0, OperatingState::Fault),
        (4.0, OperatingState::Offline),
    ] {
        assert_eq!(operating_state_from_f64(raw).unwrap(), expected);
    }
}

#[test]
fn operating_state_reads_our_labels() {
    // Typed publishing sends the state as its template label, not a code
    for (label, expected) in [
        ("STANDBY", OperatingState::Standby),
        ("CHARGING", OperatingState::Charging),
        ("DISCHARGING", OperatingState::Discharging),
        ("FAULT", OperatingState::Fault),
        ("OFFLINE", OperatingState::Offline),
    ] {
        assert_eq!(operating_state(&json!(label)).unwrap(), expected);
    }
    assert!(operating_state(&json!("ON_FIRE")).is_err());
}

#[test]
fn operating_state_from_f64_rejects_out_of_range() {
    assert!(operating_state_from_f64(5.0).is_err());
}

#[test]
fn compute_shares_splits_equally_across_two_standby_children() {
    // Arrange — two racks, equal SoC, equal_split policy.
    let cache = new_input_cache();
    for rack in ["rack_1", "rack_2"] {
        cache.insert(
            format!("sites/local_site/devices/{rack}/measurements/operating_state/none"),
            (json!(0.0), Instant::now()),
        );
        cache.insert(
            format!("sites/local_site/devices/{rack}/measurements/state_of_charge/percent"),
            (json!(50.0), Instant::now()),
        );
    }
    let child = |id: &str| ChildAllocation {
        device_id: id.to_string(),
        operating_state_topic: format!(
            "sites/{{site_id}}/devices/{id}/measurements/operating_state/none"
        ),
        state_of_charge_topic: format!(
            "sites/{{site_id}}/devices/{id}/measurements/state_of_charge/percent"
        ),
        power_min: -4_000_000.0,
        power_max: 4_000_000.0,
    };
    let binding = DistributeBinding {
        allocation_policy: "equal_split".to_string(),
        children: vec![child("rack_1"), child("rack_2")],
        state_of_charge_floor_percent: None,
        ramp_rate_per_sec: None,
        hysteresis_margin: None,
        hysteresis_dwell_secs: None,
        power_min: None,
        power_max: None,
        import_limit_topic: None,
        export_limit_topic: None,
        active_power_topic: None,
        poi_active_power_topic: None,
    };
    // Act
    let mut shares = compute_shares(&binding, 200_000.0, "local_site", &cache).unwrap();
    shares.sort_by(|a, b| a.0.cmp(&b.0));
    // Assert
    assert_eq!(
        shares,
        vec![
            ("rack_1".to_string(), 100_000.0),
            ("rack_2".to_string(), 100_000.0)
        ]
    );
}

/// Two STANDBY racks at the given SoCs, equal_split, 25% reserve floor.
fn floored_pair(soc_1: f64, soc_2: f64) -> (DistributeBinding, InputCache) {
    let cache = new_input_cache();
    for (rack, soc) in [("rack_1", soc_1), ("rack_2", soc_2)] {
        cache.insert(
            format!("sites/local_site/devices/{rack}/measurements/operating_state/none"),
            (json!(0.0), Instant::now()),
        );
        cache.insert(
            format!("sites/local_site/devices/{rack}/measurements/state_of_charge/percent"),
            (json!(soc), Instant::now()),
        );
    }
    let child = |id: &str| ChildAllocation {
        device_id: id.to_string(),
        operating_state_topic: format!(
            "sites/{{site_id}}/devices/{id}/measurements/operating_state/none"
        ),
        state_of_charge_topic: format!(
            "sites/{{site_id}}/devices/{id}/measurements/state_of_charge/percent"
        ),
        power_min: -4_000_000.0,
        power_max: 4_000_000.0,
    };
    let binding = DistributeBinding {
        allocation_policy: "equal_split".to_string(),
        children: vec![child("rack_1"), child("rack_2")],
        state_of_charge_floor_percent: Some(25.0),
        ramp_rate_per_sec: None,
        hysteresis_margin: None,
        hysteresis_dwell_secs: None,
        power_min: None,
        power_max: None,
        import_limit_topic: None,
        export_limit_topic: None,
        active_power_topic: None,
        poi_active_power_topic: None,
    };
    (binding, cache)
}

fn sorted_shares(
    binding: &DistributeBinding,
    target: f64,
    cache: &InputCache,
) -> Vec<(String, f64)> {
    let mut shares = compute_shares(binding, target, "local_site", cache).unwrap();
    shares.sort_by(|a, b| a.0.cmp(&b.0));
    shares
}

#[test]
fn discharge_skips_a_rack_at_the_reserve_floor() {
    // Arrange — rack_1 exactly at the 25% floor, rack_2 well above it
    let (binding, cache) = floored_pair(25.0, 60.0);
    // Act
    let shares = sorted_shares(&binding, 200_000.0, &cache);
    // Assert — rack_1 holds its reserve, rack_2 covers the full request
    assert_eq!(
        shares,
        vec![
            ("rack_1".to_string(), 0.0),
            ("rack_2".to_string(), 200_000.0)
        ]
    );
}

#[test]
fn discharge_stops_when_every_rack_is_at_the_floor() {
    // Arrange
    let (binding, cache) = floored_pair(24.0, 25.0);
    // Act
    let shares = sorted_shares(&binding, 200_000.0, &cache);
    // Assert — nothing discharges; the shortfall is der-control-api's to report
    assert_eq!(
        shares,
        vec![("rack_1".to_string(), 0.0), ("rack_2".to_string(), 0.0)]
    );
}

#[test]
fn charging_is_never_restricted_by_the_reserve_floor() {
    // Arrange — both racks below the floor
    let (binding, cache) = floored_pair(10.0, 20.0);
    // Act — charge 200kW
    let shares = sorted_shares(&binding, -200_000.0, &cache);
    // Assert — normal equal split
    assert_eq!(
        shares,
        vec![
            ("rack_1".to_string(), -100_000.0),
            ("rack_2".to_string(), -100_000.0)
        ]
    );
}
