//! Distribute-binding dispatch: resolve a `bess_module` setpoint into
//! per-child writes via the max-min fair allocation algorithm.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::{DistributeBinding, ProtocolBinding};
use crate::config::GatewayCredentials;
use crate::dispatch::allocation::{self, AllocationPolicy, ChildCapacity, OperatingState};
use crate::modbus::client as modbus;
use crate::synthetic::{InputCache, as_number};
use anyhow::{Context, Result, anyhow};
use serde_json::Value;
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
        .map(|child| child.map(|c| apply_reserve_floor(binding, c, target)))
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
    let operating_state = cache
        .get(&operating_state_topic)
        .map(|e| operating_state(&e.0))
        .ok_or_else(|| anyhow!("no cached operating_state for {}", c.device_id))??;
    let state_of_charge = cache
        .get(&state_of_charge_topic)
        .and_then(|e| as_number(&e.0))
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

/// Zero a child's discharge headroom once its SoC is at or below the
/// binding's reserve floor, so allocation hands its share to children still
/// above it. Charging is never restricted.
///
/// Reason: enforced per child, so every child keeps floor% of its own
/// capacity and the site total can never dip below the site-wide reserve.
/// Residual overshoot: a child crossing the floor mid-command keeps
/// discharging until the next 1 Hz rebalance tick recomputes shares, plus
/// however stale the BMS's SoC reading is (~1 s × that child's power).
fn apply_reserve_floor(
    binding: &DistributeBinding,
    child: ChildCapacity,
    target: f64,
) -> ChildCapacity {
    match binding.state_of_charge_floor_percent {
        Some(floor) if target > 0.0 && child.state_of_charge <= floor => ChildCapacity {
            headroom: 0.0,
            ..child
        },
        _ => child,
    }
}

/// A cached `operating_state` as its enum: our label (`"FAULT"`) as typed
/// publishing sends it, or the register code a number-only publish sends.
fn operating_state(value: &Value) -> Result<OperatingState> {
    match value.as_str() {
        Some("STANDBY") => Ok(OperatingState::Standby),
        Some("CHARGING") => Ok(OperatingState::Charging),
        Some("DISCHARGING") => Ok(OperatingState::Discharging),
        Some("FAULT") => Ok(OperatingState::Fault),
        Some("OFFLINE") => Ok(OperatingState::Offline),
        Some(other) => Err(anyhow!("unknown operating_state label: {other}")),
        None => operating_state_from_f64(
            as_number(value).ok_or_else(|| anyhow!("operating_state is {value}"))?,
        ),
    }
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
#[path = "distribute_test.rs"]
mod tests;
