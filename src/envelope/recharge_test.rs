//! Recharge toward readiness: only when idle, below readiness and between
//! events. High-risk: charging when nothing asked for it imports power the
//! site never committed to.

use super::requested_w;
use crate::asyncapi::types::ProtocolBinding;
use crate::synthetic::{InputCache, new_input_cache};
use serde_json::{Value, json};
use std::time::Instant;

const SOC: &str = "sites/s/devices/bess_module_1/measurements/state_of_charge/percent";
const EVENT: &str = "sites/s/devices/der_dispatch/measurements/event_active/none";

fn module(extra: Value) -> ProtocolBinding {
    let mut b = json!({
        "protocol": "distribute", "allocation_policy": "soc_weighted", "children": [],
    });
    b.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(b).unwrap()
}

/// Readiness 91.8%, recharge 248.2 kW.
fn obligated() -> ProtocolBinding {
    module(json!({ "readiness_soc_percent": 91.8, "recharge_power_w": 248_200.0 }))
}

fn site(soc: f64, event_active: f64) -> InputCache {
    let cache = new_input_cache();
    cache.insert(SOC.to_string(), (json!(soc), Instant::now()));
    cache.insert(EVENT.to_string(), (json!(event_active), Instant::now()));
    cache
}

fn requested(binding: &ProtocolBinding, operator_w: f64, cache: &InputCache) -> f64 {
    requested_w(binding, "bess_module_1", operator_w, "s", cache)
}

#[test]
fn an_idle_module_below_readiness_recharges_between_events() {
    assert_eq!(requested(&obligated(), 0.0, &site(40.0, 0.0)), -248_200.0);
}

#[test]
fn no_recharge_at_readiness_or_during_an_event() {
    assert_eq!(requested(&obligated(), 0.0, &site(91.8, 0.0)), 0.0);
    assert_eq!(requested(&obligated(), 0.0, &site(40.0, 1.0)), 0.0);
}

#[test]
fn an_operator_setpoint_always_wins() {
    let cache = site(40.0, 0.0);
    assert_eq!(requested(&obligated(), 500_000.0, &cache), 500_000.0);
    assert_eq!(requested(&obligated(), -100_000.0, &cache), -100_000.0);
}

#[test]
fn anything_missing_means_no_recharge() {
    // no obligation on the binding
    assert_eq!(requested(&module(json!({})), 0.0, &site(40.0, 0.0)), 0.0);
    // half an obligation
    let half = module(json!({ "readiness_soc_percent": 91.8 }));
    assert_eq!(requested(&half, 0.0, &site(40.0, 0.0)), 0.0);
    // event state never published
    let no_event = new_input_cache();
    no_event.insert(SOC.to_string(), (json!(40.0), Instant::now()));
    assert_eq!(requested(&obligated(), 0.0, &no_event), 0.0);
    // SoC never published
    let no_soc = new_input_cache();
    no_soc.insert(EVENT.to_string(), (json!(0.0), Instant::now()));
    assert_eq!(requested(&obligated(), 0.0, &no_soc), 0.0);
}
