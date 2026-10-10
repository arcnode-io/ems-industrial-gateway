//! When a derived value's input counts as dead. A derived value with a dead
//! input isn't published: a sum over readings that stopped would show a
//! steady load that isn't there.

use crate::app::{DEFAULT_POLL_HZ, MAX_POLL_HZ, MIN_POLL_HZ};
use crate::asyncapi::types::AsyncApiSpec;
use std::collections::HashMap;
use std::time::Duration;

/// Never call an input dead sooner than this.
const STALE_FLOOR: Duration = Duration::from_secs(5);
/// Polls an input may miss before it counts as dead.
const MISSED_POLLS: f64 = 3.0;

/// Per-input dead limits for `input_topics` (site-substituted measurement
/// topics): 3 missed polls, never under 5 s. Only inputs this gateway polls
/// get one; Reason: another service's on-change topics (envelope limits) go
/// quiet by design, and that service reports its own liveness.
pub fn stale_limits(spec: &AsyncApiSpec, input_topics: &[String]) -> HashMap<String, Duration> {
    input_topics
        .iter()
        .filter_map(|t| {
            let hz = poll_rate_hz(spec, t)?;
            Some((
                t.clone(),
                STALE_FLOOR.max(Duration::from_secs_f64(MISSED_POLLS / hz)),
            ))
        })
        .collect()
}

/// The spec's poll rate for a `sites/{site}/devices/{device}/measurements/{m}/{unit}`
/// topic, as the poll loop clamps it; `None` when the spec doesn't poll it
/// (e.g. another publisher's).
fn poll_rate_hz(spec: &AsyncApiSpec, topic: &str) -> Option<f64> {
    let parts: Vec<&str> = topic.split('/').collect();
    let [_, _, _, device, _, measurement, _] = parts[..] else {
        return None;
    };
    spec.x_protocol_source
        .get(device)?
        .get(measurement)?
        .poll_rate_hz
        .unwrap_or(DEFAULT_POLL_HZ)
        .clamp(MIN_POLL_HZ, MAX_POLL_HZ)
        .into()
}

#[cfg(test)]
#[path = "stale_test.rs"]
mod tests;
