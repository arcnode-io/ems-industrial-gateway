//! Racks the gateway can't use get an explicit 0 W, never their last
//! setpoint. High-risk: a rack left out of a split keeps doing whatever it
//! was last told, unseen.

use super::compute_shares;
use crate::asyncapi::types::DistributeBinding;
use crate::synthetic::{InputCache, new_input_cache};
use serde_json::json;
use std::time::{Duration, Instant};

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

/// Rack `n`'s state and SoC, received at `at`.
fn heard(cache: &InputCache, n: u32, state: &str, at: Instant) {
    cache.insert(
        format!("sites/s/devices/rack_{n}/state"),
        (json!(state), at),
    );
    cache.insert(format!("sites/s/devices/rack_{n}/soc"), (json!(60.0), at));
}

fn stale() -> Instant {
    Instant::now() - Duration::from_secs(30)
}

#[test]
fn a_rack_gone_quiet_gets_zero_and_its_share_moves() {
    // Arrange — rack_1 last heard 30 s ago
    let cache = new_input_cache();
    heard(&cache, 1, "DISCHARGING", stale());
    heard(&cache, 2, "DISCHARGING", Instant::now());
    // Act
    let mut shares =
        compute_shares(&module(), 800_000.0, "s", &cache, &Default::default()).unwrap();
    shares.sort_by(|a, b| a.0.cmp(&b.0));
    // Assert
    assert_eq!(
        shares,
        vec![
            ("rack_1".to_string(), 0.0),
            ("rack_2".to_string(), 800_000.0)
        ]
    );
}

#[test]
fn a_faulted_rack_is_told_zero_not_left_alone() {
    let cache = new_input_cache();
    heard(&cache, 1, "FAULT", Instant::now());
    heard(&cache, 2, "DISCHARGING", Instant::now());
    let shares = compute_shares(&module(), 800_000.0, "s", &cache, &Default::default()).unwrap();
    assert!(shares.contains(&("rack_1".to_string(), 0.0)), "{shares:?}");
}

#[test]
fn with_every_rack_quiet_the_module_goes_to_zero() {
    let cache = new_input_cache();
    heard(&cache, 1, "DISCHARGING", stale());
    heard(&cache, 2, "DISCHARGING", stale());
    let shares = compute_shares(&module(), 800_000.0, "s", &cache, &Default::default()).unwrap();
    assert!(
        shares.iter().all(|(_, w)| *w == 0.0) && shares.len() == 2,
        "{shares:?}"
    );
}

#[test]
fn a_rack_never_heard_from_still_holds() {
    // startup: no reading yet is not a reading gone stale
    let cache = new_input_cache();
    heard(&cache, 2, "DISCHARGING", Instant::now());
    assert!(compute_shares(&module(), 800_000.0, "s", &cache, &Default::default()).is_err());
}
