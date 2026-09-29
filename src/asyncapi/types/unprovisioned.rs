//! Skip unprovisioned devices while parsing the spec's source maps.
//!
//! A device whose address the utility hasn't assigned yet carries the
//! `PROVISIONED_AT_COMMISSIONING` sentinel in its connection fields, which
//! device-api passes through as-is. Parsed strictly, that string fails the
//! typed `port`, which failed the entire spec: one unprovisioned device kept
//! every other device offline. Devices come online one commissioning POST at
//! a time, so an unprovisioned entry is skipped (no poll or write task) and
//! picked up on the reconcile after its address lands. Anything else that
//! doesn't parse is still a real error.

use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer, Error as _};
use serde_json::Value;
use std::collections::HashMap;
use tracing::info;

/// Placeholder edp-api emits for connection fields assigned at commissioning.
const SENTINEL: &str = "PROVISIONED_AT_COMMISSIONING";

/// Per-device, per-channel source map with unprovisioned entries dropped.
pub fn deserialize<'de, D, T>(d: D) -> Result<HashMap<String, HashMap<String, T>>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let raw = HashMap::<String, HashMap<String, Value>>::deserialize(d)?;
    let mut out = HashMap::with_capacity(raw.len());
    for (device_id, channels) in raw {
        if channels.values().any(is_unprovisioned) {
            info!(%device_id, "device unprovisioned (address assigned at commissioning); skipping");
            continue;
        }
        let parsed = channels
            .into_iter()
            .map(|(name, entry)| {
                serde_json::from_value(entry)
                    .map(|typed| (name.clone(), typed))
                    .map_err(|e| D::Error::custom(format!("{device_id}.{name}: {e}")))
            })
            .collect::<Result<_, _>>()?;
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
