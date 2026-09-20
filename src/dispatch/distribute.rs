//! Distribute-binding dispatch: resolve a `bess_module` setpoint into
//! per-child writes via the max-min fair allocation algorithm.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::{DistributeBinding, ProtocolBinding};
use crate::config::GatewayCredentials;
use crate::dispatch::allocation::{self, AllocationPolicy, ChildCapacity, OperatingState};
use crate::modbus::client as modbus;
use crate::synthetic::InputCache;
use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;

/// Resolve + execute a distribute command: `compute_shares` then
/// `write_shares` for every child. The reactive path (a real inbound
/// command) always writes every eligible child; the rebalance-tick path
/// (`envelope::task`) calls the two halves separately so it can write only
/// the children whose share actually changed since the last tick.
#[allow(clippy::too_many_arguments)]
pub async fn dispatch_distribute(
    binding: &DistributeBinding,
    target: f64,
    channel_key: &str,
    site_id: &str,
    device_channels: &HashMap<String, HashMap<String, ProtocolBinding>>,
    device_trust: &HashMap<String, DeviceTrust>,
    creds: Option<&GatewayCredentials>,
    cache: &InputCache,
) -> Result<()> {
    let shares = compute_shares(binding, target, site_id, cache)?;
    write_shares(&shares, channel_key, device_channels, device_trust, creds).await
}

/// Read each child's cached `operating_state`/`state_of_charge` and allocate
/// `target` across eligible children (max-min fair share). Pure aside from
/// the cache reads — no I/O, no writes. Errors if resolving any child's cache
/// entry fails, or no child ends up eligible.
pub fn compute_shares(
    binding: &DistributeBinding,
    target: f64,
    site_id: &str,
    cache: &InputCache,
) -> Result<Vec<(String, f64)>> {
    let policy = AllocationPolicy::parse(&binding.allocation_policy)?;
    let children: Vec<ChildCapacity> = binding
        .children
        .iter()
        .map(|c| resolve_child(c, target, site_id, cache))
        .collect::<Result<_>>()?;
    let shares = allocation::allocate(target, &children, policy);
    if shares.is_empty() {
        return Err(anyhow!("no eligible children to distribute to"));
    }
    Ok(shares)
}

/// Write each `(device_id, share)` via the same `(device_id, verb+target)` →
/// `Binding` lookup the module's own command used, since children share
/// verb+target with the module for the same physical quantity. Stops at the
/// first write failure; earlier writes in the loop have already landed —
/// N independent physical devices can't be rolled back as one transaction.
pub async fn write_shares(
    shares: &[(String, f64)],
    channel_key: &str,
    device_channels: &HashMap<String, HashMap<String, ProtocolBinding>>,
    device_trust: &HashMap<String, DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<()> {
    for (device_id, share) in shares {
        let binding = device_channels
            .get(device_id)
            .and_then(|chs| chs.get(channel_key))
            .ok_or_else(|| anyhow!("child {device_id} has no {channel_key} binding"))?;
        let ProtocolBinding::ModbusTcp(b) = binding else {
            return Err(anyhow!(
                "child {device_id}'s {channel_key} binding is not modbus_tcp"
            ));
        };
        let trust = device_trust.get(device_id);
        modbus::write_setpoint(b, *share, trust, creds)
            .await
            .with_context(|| format!("write to child {device_id} failed"))?;
    }
    Ok(())
}

/// Read one child's cached measurements into `ChildCapacity`. `headroom` is
/// the static bound in the direction of `target` — `power_max` when
/// discharging/positive, `|power_min|` when charging/negative. `site_id`
/// resolves the `{site_id}` placeholder in the child's cache topics — the
/// same runtime substitution `app.rs` applies when building the
/// subscription list these topics were cached under.
fn resolve_child(
    c: &crate::asyncapi::types::ChildAllocation,
    target: f64,
    site_id: &str,
    cache: &InputCache,
) -> Result<ChildCapacity> {
    let operating_state_topic = c.operating_state_topic.replace("{site_id}", site_id);
    let state_of_charge_topic = c.state_of_charge_topic.replace("{site_id}", site_id);
    let raw_state = cache
        .get(&operating_state_topic)
        .map(|e| e.0)
        .ok_or_else(|| anyhow!("no cached operating_state for {}", c.device_id))?;
    let operating_state = operating_state_from_f64(raw_state)?;
    let state_of_charge = cache
        .get(&state_of_charge_topic)
        .map(|e| e.0)
        .ok_or_else(|| anyhow!("no cached state_of_charge for {}", c.device_id))?;
    let headroom = if target < 0.0 {
        c.power_min.abs()
    } else {
        c.power_max
    };
    Ok(ChildCapacity {
        device_id: c.device_id.clone(),
        operating_state,
        headroom,
        state_of_charge,
    })
}

/// Map a cached `operating_state` reading back to its enum. Register-value
/// convention per `bess_rack.yaml`: 0=STANDBY, 1=CHARGING, 2=DISCHARGING,
/// 3=FAULT, 4=OFFLINE.
fn operating_state_from_f64(raw: f64) -> Result<OperatingState> {
    #[allow(clippy::cast_possible_truncation)]
    match raw.round() as i64 {
        0 => Ok(OperatingState::Standby),
        1 => Ok(OperatingState::Charging),
        2 => Ok(OperatingState::Discharging),
        3 => Ok(OperatingState::Fault),
        4 => Ok(OperatingState::Offline),
        other => Err(anyhow!("unknown operating_state value: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asyncapi::types::ChildAllocation;
    use crate::synthetic::new_input_cache;
    use std::time::Instant;

    #[test]
    fn resolve_child_substitutes_site_id_before_cache_lookup() {
        // Arrange — cache keyed by the RESOLVED topic (site_id substituted),
        // matching what app.rs's subscription list actually caches under.
        let cache = new_input_cache();
        cache.insert(
            "sites/site_001/devices/rack_1/measurements/operating_state/none".into(),
            (0.0, Instant::now()),
        );
        cache.insert(
            "sites/site_001/devices/rack_1/measurements/state_of_charge/percent".into(),
            (65.0, Instant::now()),
        );
        let child = ChildAllocation {
            device_id: "rack_1".to_string(),
            operating_state_topic:
                "sites/{site_id}/devices/rack_1/measurements/operating_state/none".to_string(),
            state_of_charge_topic:
                "sites/{site_id}/devices/rack_1/measurements/state_of_charge/percent".to_string(),
            power_min: -4_000_000.0,
            power_max: 4_000_000.0,
        };
        // Act
        let resolved = resolve_child(&child, 100.0, "site_001", &cache).unwrap();
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
    fn operating_state_from_f64_rejects_out_of_range() {
        assert!(operating_state_from_f64(5.0).is_err());
    }

    #[test]
    fn compute_shares_splits_equally_across_two_standby_children() {
        // Arrange — two racks, equal SoC, equal_split policy.
        let cache = new_input_cache();
        for rack in ["rack_1", "rack_2"] {
            cache.insert(
                format!("sites/site_001/devices/{rack}/measurements/operating_state/none"),
                (0.0, Instant::now()),
            );
            cache.insert(
                format!("sites/site_001/devices/{rack}/measurements/state_of_charge/percent"),
                (50.0, Instant::now()),
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
            ramp_rate_per_sec: None,
            hysteresis_margin: None,
            hysteresis_dwell_secs: None,
            power_min: None,
            power_max: None,
            import_limit_topic: None,
            export_limit_topic: None,
            active_power_topic: None,
        };
        // Act
        let mut shares = compute_shares(&binding, 200_000.0, "site_001", &cache).unwrap();
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
}
