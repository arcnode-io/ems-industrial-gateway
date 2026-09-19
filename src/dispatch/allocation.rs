//! Max-min fair allocation of a `bess_module` setpoint across its
//! `bess_rack` children (Bertsekas–Gallager max-min fair share — the same
//! algorithm networking uses for bandwidth allocation under per-flow caps).
//! Pure: no MQTT/Modbus. Callers resolve cache reads into `ChildCapacity`
//! and turn the returned shares into writes.

use anyhow::{Result, anyhow};

/// Rack operating state. Only `Fault`/`Offline` matter here — either makes
/// a child ineligible for allocation regardless of policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatingState {
    /// Idle, no power flow.
    Standby,
    /// Actively charging.
    Charging,
    /// Actively discharging.
    Discharging,
    /// Faulted — excluded from allocation.
    Fault,
    /// Offline — excluded from allocation.
    Offline,
}

impl OperatingState {
    /// Whether this state can receive an allocated share.
    fn is_eligible(self) -> bool {
        !matches!(self, Self::Fault | Self::Offline)
    }
}

/// One child's allocation inputs.
#[derive(Debug, Clone)]
pub struct ChildCapacity {
    /// Device id — carried through so the caller can map a share back to a write.
    pub device_id: String,
    /// Current operating state; `Fault`/`Offline` are skipped unconditionally.
    pub operating_state: OperatingState,
    /// Non-negative remaining room in the direction of the requested total
    /// (the caller resolves the sign — e.g. `bounds.max - current` when
    /// discharging, `current - bounds.min` when charging).
    pub headroom: f64,
    /// Current state of charge, percent (0-100). Only read by `SocWeighted`.
    pub state_of_charge: f64,
}

/// Allocation policy, matching the `distribute` binding's `allocation_policy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationPolicy {
    /// Every eligible child gets an equal share, subject to its own cap.
    EqualSplit,
    /// Shares are weighted by state of charge — discharge favors higher
    /// SoC, charge favors lower SoC (SoC-balancing convention).
    SocWeighted,
}

impl AllocationPolicy {
    /// Parse an allocation policy name (matches the JSON `allocation_policy`
    /// field). Unknown names = error, surfaces at gateway startup not runtime.
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "equal_split" => Ok(Self::EqualSplit),
            "soc_weighted" => Ok(Self::SocWeighted),
            other => Err(anyhow!("unknown allocation policy: {other}")),
        }
    }
}

/// Allocate `total` across `children` per `policy`. Skips `Fault`/`Offline`
/// children unconditionally. Clamps each child's share to its own
/// `headroom`; any clamped-off remainder is redistributed among children
/// still under their own cap, iterating to convergence (max-min fair
/// share). Returns `(device_id, allocated_share)` for eligible children
/// only — the share carries the same sign as `total`.
pub fn allocate(
    total: f64,
    children: &[ChildCapacity],
    policy: AllocationPolicy,
) -> Vec<(String, f64)> {
    let eligible: Vec<&ChildCapacity> = children
        .iter()
        .filter(|c| c.operating_state.is_eligible())
        .collect();
    if eligible.is_empty() {
        return Vec::new();
    }

    let sign = if total < 0.0 { -1.0 } else { 1.0 };
    let weights = policy_weights(policy, &eligible, sign);

    let mut shares = vec![0.0_f64; eligible.len()];
    let mut open: Vec<usize> = (0..eligible.len()).collect();
    let mut demand = total.abs();

    while demand > f64::EPSILON && !open.is_empty() {
        let weight_sum: f64 = open.iter().map(|&i| weights[i]).sum();
        let proposed: Vec<(usize, f64)> = open
            .iter()
            .map(|&i| {
                let w = if weight_sum > 0.0 {
                    weights[i] / weight_sum
                } else {
                    1.0 / open.len() as f64
                };
                (i, demand * w)
            })
            .collect();

        let mut still_open = Vec::new();
        let mut consumed = 0.0;
        let mut any_capped = false;
        for (i, share) in &proposed {
            let cap = eligible[*i].headroom.max(0.0);
            if *share >= cap {
                shares[*i] = cap;
                consumed += cap;
                any_capped = true;
            } else {
                still_open.push(*i);
            }
        }

        if any_capped {
            demand -= consumed;
            open = still_open;
        } else {
            for (i, share) in proposed {
                shares[i] = share;
            }
            demand = 0.0;
        }
    }

    eligible
        .iter()
        .zip(shares)
        .map(|(c, s)| (c.device_id.clone(), sign * s))
        .collect()
}

/// Per-child weight for `policy`, indexed the same as `eligible`.
/// `SocWeighted` falls back to equal weights when its weighted denominator
/// is ~0 (e.g. every eligible child at 100% SoC during a charge command) —
/// dividing by that would otherwise NaN the allocation.
fn policy_weights(policy: AllocationPolicy, eligible: &[&ChildCapacity], sign: f64) -> Vec<f64> {
    match policy {
        AllocationPolicy::EqualSplit => vec![1.0; eligible.len()],
        AllocationPolicy::SocWeighted => {
            // Discharge (sign > 0): higher SoC contributes more.
            // Charge (sign < 0): lower SoC (more headroom-to-full) absorbs more.
            let raw: Vec<f64> = eligible
                .iter()
                .map(|c| {
                    if sign > 0.0 {
                        c.state_of_charge
                    } else {
                        100.0 - c.state_of_charge
                    }
                })
                .collect();
            if raw.iter().sum::<f64>() <= f64::EPSILON {
                vec![1.0; eligible.len()]
            } else {
                raw
            }
        }
    }
}
