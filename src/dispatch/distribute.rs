//! Distribute-binding dispatch: resolve a `bess_module` setpoint into
//! per-child writes via the max-min fair allocation algorithm.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::{DistributeBinding, ProtocolBinding};
use crate::config::GatewayCredentials;
use crate::dispatch::allocation::{self, AllocationPolicy, ChildCapacity, OperatingState};
use crate::dispatch::operating_state::operating_state;
use crate::dispatch::rack_limits::{self, STALE_AFTER};
use crate::dispatch::reserve;
use crate::envelope;
use crate::modbus::client as modbus;
use crate::synthetic::{InputCache, as_number};
use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;
use tracing::info;

/// Resolve + execute a distribute command: `compute_shares` then
/// `write_shares` for every child. On an envelope-guarded module, nothing:
/// the command is already the module's requested setpoint (`handle_command`
/// recorded it), and its envelope task writes it, clamped to the limits.
/// Reason: a direct write bypasses the envelope; a dispatch landing while it
/// binds yanked the battery off what the limit needed. The reactive path (a real inbound
/// command) always writes every eligible child; the rebalance-tick path
/// (`envelope::task`) calls the two halves separately so it can order a
/// handoff's writes.
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
    if envelope::envelope_guard_config(binding).is_some() {
        info!(
            target,
            "envelope-guarded: its envelope task writes the racks"
        );
        return Ok(());
    }
    let shares = compute_shares(binding, target, site_id, cache)?;
    write_shares(&shares, channel_key, device_channels, device_trust, creds).await?;
    if target != 0.0 && shares.iter().all(|(_, w)| *w == 0.0) {
        return Err(anyhow!("no rack could take {target} W; all held at 0 W"));
    }
    Ok(())
}

/// Read each child's cached `operating_state`/`state_of_charge` and allocate
/// `target` across eligible children (max-min fair share); every other child
/// gets 0 W. Pure aside from the cache reads: no I/O, no writes. Errors if a
/// child has never reported.
pub fn compute_shares(
    binding: &DistributeBinding,
    target: f64,
    site_id: &str,
    cache: &InputCache,
) -> Result<Vec<(String, f64)>> {
    let policy = AllocationPolicy::parse(&binding.allocation_policy)?;
    let floor = reserve::effective_floor_percent(binding, site_id, cache);
    let children: Vec<ChildCapacity> = binding
        .children
        .iter()
        .map(|c| resolve_child(c, target, site_id, cache))
        .map(|child| child.map(|c| apply_reserve_floor(floor, c, target)))
        .collect::<Result<_>>()?;
    let mut shares = allocation::allocate(target, &children, policy);
    // Reason: a child left out of the split keeps its last setpoint, unseen.
    // Every child it can't use is told 0 W.
    for c in &children {
        if shares.iter().all(|(id, _)| *id != c.device_id) {
            shares.push((c.device_id.clone(), 0.0));
        }
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
/// its room in the direction of `target`: the static bound (`power_max`
/// discharging, `|power_min|` charging) capped by its live limit
/// (`rack_limits`). `site_id`
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
    let state_entry = cache
        .get(&operating_state_topic)
        .ok_or_else(|| anyhow!("no cached operating_state for {}", c.device_id))?;
    let soc_entry = cache
        .get(&state_of_charge_topic)
        .ok_or_else(|| anyhow!("no cached state_of_charge for {}", c.device_id))?;
    let state_of_charge =
        as_number(&soc_entry.0).ok_or_else(|| anyhow!("state_of_charge for {}", c.device_id))?;
    // Reason: a rack that has stopped reporting can't be steered blind, so
    // it's treated as offline (0 W) until it reports again.
    let quiet = state_entry.1.elapsed() > STALE_AFTER || soc_entry.1.elapsed() > STALE_AFTER;
    let operating_state = if quiet {
        OperatingState::Offline
    } else {
        operating_state(&state_entry.0)?
    };
    Ok(ChildCapacity {
        device_id: c.device_id.clone(),
        operating_state,
        headroom: rack_limits::headroom(c, target, site_id, cache),
        state_of_charge,
    })
}

/// Zero a child's discharge headroom once its SoC is at or below the
/// effective reserve floor (`reserve`), so allocation hands its share to
/// children still above it. Charging is never restricted.
///
/// Reason: enforced per child, so every child keeps floor% of its own
/// capacity and the site total can never dip below the site-wide reserve.
/// Residual overshoot: a child crossing the floor mid-command keeps
/// discharging until the next 1 Hz rebalance tick recomputes shares, plus
/// however stale the BMS's SoC reading is (~1 s × that child's power).
fn apply_reserve_floor(floor: Option<f64>, child: ChildCapacity, target: f64) -> ChildCapacity {
    match floor {
        Some(floor) if target > 0.0 && child.state_of_charge <= floor => ChildCapacity {
            headroom: 0.0,
            ..child
        },
        _ => child,
    }
}

#[cfg(test)]
#[path = "distribute_test.rs"]
mod tests;

#[cfg(test)]
#[path = "distribute_stale_test.rs"]
mod stale_tests;
