//! The operator's requested setpoint per device + `{verb}_{target}`: the
//! target the envelope and rebalance loops steer toward.

use std::collections::HashMap;
use tokio::sync::RwLock;

/// Device → `{verb}_{target}` → requested value.
type Requested = RwLock<HashMap<String, HashMap<String, f64>>>;

/// Record `value` as `device_id`'s `key` request; returns what it replaced.
///
/// Reason: recorded before the write, not after it lands. The 1 Hz
/// rebalance tick writes the record to every child; a tick in the write
/// window would otherwise write the old record (0 W when there was none)
/// over the command that just landed.
pub async fn record(requested: &Requested, device_id: &str, key: &str, value: f64) -> Option<f64> {
    requested
        .write()
        .await
        .entry(device_id.to_string())
        .or_default()
        .insert(key.to_string(), value)
}

/// Undo a `record` whose write failed: put `previous` back (or clear the
/// key), unless a newer command has replaced `value` since.
pub async fn restore(
    requested: &Requested,
    device_id: &str,
    key: &str,
    value: f64,
    previous: Option<f64>,
) {
    let mut all = requested.write().await;
    let Some(device) = all.get_mut(device_id) else {
        return;
    };
    if device.get(key) != Some(&value) {
        return;
    }
    match previous {
        Some(p) => device.insert(key.to_string(), p),
        None => device.remove(key),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_failed_command_puts_the_last_good_request_back() {
        let requested = Requested::default();
        record(&requested, "m", "k", 1.0).await;
        let previous = record(&requested, "m", "k", 2.0).await;
        restore(&requested, "m", "k", 2.0, previous).await;
        assert_eq!(requested.read().await["m"].get("k"), Some(&1.0));
    }

    #[tokio::test]
    async fn a_failure_never_undoes_a_newer_command() {
        let requested = Requested::default();
        let previous = record(&requested, "m", "k", 2.0).await;
        record(&requested, "m", "k", 3.0).await;
        restore(&requested, "m", "k", 2.0, previous).await;
        assert_eq!(requested.read().await["m"].get("k"), Some(&3.0));
    }
}
