//! Parse the spec's source maps, skipping devices the gateway can't run.
//!
//! Two reasons to skip a device instead of failing the whole spec:
//! - Unprovisioned: its connection fields still carry the
//!   `PROVISIONED_AT_COMMISSIONING` sentinel, which device-api passes through
//!   as-is. Devices come online one commissioning POST at a time, so it's
//!   picked up on the reconcile after its address lands.
//! - Unusable Modbus binding: a measurement must read (FC3/FC4), a command
//!   must write (FC6/FC16, FC6 only for one-register types) and can't carry
//!   a SunSpec scale factor, which only reads apply. A bad template on one
//!   device mustn't take every other device offline.
//! - Unread DNP3 point type: measurements read analog or binary inputs only.
//! - A power cap with part of an envelope guard: shedding was enabled but
//!   can't run.
//!
//! Anything else that doesn't parse is still a real error.

use super::{CommandSource, PowerCapBinding, ProtocolBinding, ProtocolSource};
use crate::dnp3::master::point_kind;
use crate::modbus::codec::{read_function, write_function};
use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer, Error as _};
use serde_json::Value;
use std::collections::HashMap;
use tracing::{info, warn};

/// Placeholder edp-api emits for connection fields assigned at commissioning.
const SENTINEL: &str = "PROVISIONED_AT_COMMISSIONING";

/// Device id → channel name → entry.
type SourceMap<T> = HashMap<String, HashMap<String, T>>;

/// `x-protocol-source` with unprovisioned or unreadable devices dropped.
pub fn measurements<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<SourceMap<ProtocolSource>, D::Error> {
    parse(d, |s: &ProtocolSource| match &s.binding {
        ProtocolBinding::ModbusTcp(m) => read_function(m.function_code).map(drop),
        ProtocolBinding::Dnp3Tcp(d) => point_kind(&d.point_type).map(drop),
        _ => Ok(()),
    })
}

/// `x-command-source` with unprovisioned or unwritable devices dropped.
pub fn commands<'de, D: Deserializer<'de>>(d: D) -> Result<SourceMap<CommandSource>, D::Error> {
    parse(d, |s: &CommandSource| match &s.binding {
        ProtocolBinding::ModbusTcp(m) if m.scale_factor_address.is_some() => {
            Err("scale_factor_address (SunSpec sunssf) is only applied on reads".to_string())
        }
        ProtocolBinding::ModbusTcp(m) => write_function(m.function_code, m.data_type).map(drop),
        ProtocolBinding::PowerCap(p) => whole_guard(p),
        _ => Ok(()),
    })
}

/// A power cap's envelope guard is all six fields or none. Part of one means
/// the site enabled compute shedding but the gateway couldn't run it.
fn whole_guard(p: &PowerCapBinding) -> Result<(), String> {
    let present = [
        p.import_limit_topic.is_some(),
        p.export_limit_topic.is_some(),
        p.poi_active_power_topic.is_some(),
        p.hysteresis_margin.is_some(),
        p.hysteresis_dwell_secs.is_some(),
        p.ramp_rate_per_sec.is_some(),
    ];
    match present.iter().filter(|&&f| f).count() {
        0 | 6 => Ok(()),
        n => Err(format!("power_cap envelope guard has {n} of its 6 fields")),
    }
}

/// Shared walk: skip unprovisioned devices, type each entry, then skip the
/// device if `check` rejects any of its entries.
fn parse<'de, D, T>(d: D, check: fn(&T) -> Result<(), String>) -> Result<SourceMap<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let raw = SourceMap::<Value>::deserialize(d)?;
    let mut out = HashMap::with_capacity(raw.len());
    for (device_id, channels) in raw {
        if channels.values().any(is_unprovisioned) {
            info!(%device_id, "device unprovisioned (address assigned at commissioning); skipping");
            continue;
        }
        let parsed: HashMap<String, T> = channels
            .into_iter()
            .map(|(name, entry)| {
                serde_json::from_value(entry)
                    .map(|typed| (name.clone(), typed))
                    .map_err(|e| D::Error::custom(format!("{device_id}.{name}: {e}")))
            })
            .collect::<Result<_, _>>()?;
        if let Some((name, reason)) = parsed
            .iter()
            .find_map(|(name, entry)| check(entry).err().map(|r| (name, r)))
        {
            warn!(%device_id, channel = %name, %reason, "unusable binding; skipping device");
            continue;
        }
        out.insert(device_id, parsed);
    }
    Ok(out)
}

/// Whether an entry's connection still carries the commissioning sentinel.
fn is_unprovisioned(entry: &Value) -> bool {
    ["host", "port", "unit_id"]
        .iter()
        .any(|field| entry.get(field).and_then(Value::as_str) == Some(SENTINEL))
}
