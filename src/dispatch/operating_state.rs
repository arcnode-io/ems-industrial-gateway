//! A rack's cached `operating_state` as its enum, whether typed publishing
//! sent our label or a number-only publish sent the register code.

use crate::dispatch::allocation::OperatingState;
use crate::synthetic::as_number;
use anyhow::{Result, anyhow};
use serde_json::Value;

/// A cached `operating_state` as its enum: our label (`"FAULT"`) as typed
/// publishing sends it, or the register code a number-only publish sends.
pub(super) fn operating_state(value: &Value) -> Result<OperatingState> {
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
pub(super) fn operating_state_from_f64(raw: f64) -> Result<OperatingState> {
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
