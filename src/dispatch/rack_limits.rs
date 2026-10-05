//! A rack's present charge/discharge limits (bess_rack `max_charge_power`,
//! `max_discharge_power`, positive W) cap its static bound. They shrink at
//! the SoC ends, where the static bound over-promises.
//!
//! Topics come from the rack's device_id by the platform topic scheme. A
//! rack without a fresh reading (template doesn't declare it, not polled
//! yet, poll failing) falls back to its static bound, with a WARN on the
//! switch.

use crate::asyncapi::types::ChildAllocation;
use crate::synthetic::{InputCache, as_number};
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tracing::{info, warn};

/// A limit older than this (5 missed 1 Hz polls) counts as missing.
pub const STALE_AFTER: Duration = Duration::from_secs(5);

/// `max_charge_power` / `max_discharge_power` topic for rack `device_id`.
pub fn limit_topic(site_id: &str, device_id: &str, charging: bool) -> String {
    let name = if charging {
        "max_charge_power"
    } else {
        "max_discharge_power"
    };
    format!("sites/{site_id}/devices/{device_id}/measurements/{name}/watts")
}

/// Room the rack has in `target`'s direction, W: its static bound, capped
/// by its live limit when that's fresh.
pub fn headroom(c: &ChildAllocation, target: f64, site_id: &str, cache: &InputCache) -> f64 {
    let charging = target < 0.0;
    let static_w = if charging {
        c.power_min.abs()
    } else {
        c.power_max
    };
    let topic = limit_topic(site_id, &c.device_id, charging);
    let live = cache
        .get(&topic)
        .filter(|e| e.1.elapsed() <= STALE_AFTER)
        .and_then(|e| as_number(&e.0));
    note_fallback(&topic, live.is_none());
    live.map_or(static_w, |limit| static_w.min(limit.max(0.0)))
}

/// Limit topics currently falling back to the static bound.
///
/// Reason: rebalance runs at 1 Hz per module; logging only the switch keeps
/// a missing limit visible without a WARN every second.
static FALLING_BACK: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

/// Log when `topic` starts or stops falling back.
fn note_fallback(topic: &str, missing: bool) {
    let mut set = FALLING_BACK.lock().expect("fallback set poisoned");
    if missing && set.insert(topic.to_string()) {
        warn!(%topic, "rack limit missing or stale; using its static bound");
    } else if !missing && set.remove(topic) {
        info!(%topic, "rack limit live again");
    }
}

#[cfg(test)]
#[path = "rack_limits_test.rs"]
mod tests;
