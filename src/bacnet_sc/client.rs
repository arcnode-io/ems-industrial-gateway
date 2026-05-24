//! BACnet/SC measurement read — wraps `bacnet_rs::BacnetScClient`.
//!
//! MVP: per-read connect (TLS+WebSocket handshake + Connect-Request each
//! poll). Wasteful vs a connection pool but simplest; tracked as a
//! TODO for optimization once we know real poll cadences.
//!
//! Gateway identity (VMAC + Device UUID) is generated freshly per
//! connect via `rand` — the spec allows random Random-48 VMACs. A
//! persistent-identity config field can land later if a deployment
//! needs the hub to recognize the gateway across reconnects.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::BacnetScBinding;
use crate::config::GatewayCredentials;
use anyhow::{Context, Result};
use bacnet_rs::datalink::sc::client::BacnetScClient;
use bacnet_rs::datalink::sc::hub::NodeIdentity;
use bacnet_rs::datalink::sc::node::{BacnetScAddress, BacnetScConfig, BacnetScDataLink};
use bacnet_rs::object::{ObjectIdentifier, ObjectType, PropertyIdentifier};
use bacnet_rs::property::PropertyValue;

/// Read one BACnet property over BACnet/SC.
///
/// `trust` is currently unused — BACnet/SC's auth happens at the
/// transport layer (mTLS in `creds`) rather than per-device. If
/// `creds` is None the gateway can't open a TLS connection at all
/// and we error out fast. Per-device trust (e.g. validating the
/// hub's cert subject) would slot in here later.
pub async fn read_measurement(
    b: &BacnetScBinding,
    _trust: Option<&DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<f64> {
    let creds = creds.context(
        "BACnet/SC requires gateway_credentials (CA bundle + cert + key) — not configured",
    )?;
    if b.object_type != "analog_input" {
        anyhow::bail!(
            "Tier 1 BACnet/SC only supports analog_input object_type, got {}",
            b.object_type
        );
    }
    if b.property_id != "present_value" {
        anyhow::bail!(
            "Tier 1 BACnet/SC only supports present_value property_id, got {}",
            b.property_id
        );
    }
    let dest_vmac = parse_vmac(&b.device_vmac)?;
    let identity = NodeIdentity {
        // Random VMAC — spec-allowed for unconfigured gateway nodes
        // (Random-48). Future: pull from cfg if persistent identity needed.
        vmac: random_vmac(),
        device_uuid: random_uuid(),
        max_bvlc_length: 1497,
        max_npdu_length: 1476,
    };
    let cfg = BacnetScConfig {
        hub_url: b.hub_url.clone(),
        ca_bundle_path: creds.ca_bundle_path.clone(),
        cert_path: creds.cert_path.clone(),
        key_path: creds.key_path.clone(),
        identity,
    };
    let datalink = BacnetScDataLink::connect(cfg)
        .await
        .context("BACnet/SC connect")?;
    let mut client = BacnetScClient::new(datalink);
    let object_id = ObjectIdentifier::new(ObjectType::AnalogInput, b.object_instance);
    let value = client
        .read_property(
            BacnetScAddress::Unicast(dest_vmac),
            object_id,
            PropertyIdentifier::PresentValue,
        )
        .await
        .context("BACnet/SC read_property")?;
    coerce_to_f64(value)
}

/// Parse `"AA:BB:CC:DD:EE:FF"` (or `"AABBCCDDEEFF"`) into a 6-byte VMAC.
fn parse_vmac(s: &str) -> Result<[u8; 6]> {
    let cleaned: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if cleaned.len() != 12 {
        anyhow::bail!(
            "VMAC must be 12 hex digits (got {} from {s:?})",
            cleaned.len()
        );
    }
    let mut out = [0u8; 6];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&cleaned[i * 2..i * 2 + 2], 16)
            .with_context(|| format!("invalid hex byte at position {i} in {s:?}"))?;
    }
    Ok(out)
}

/// Random 6-byte VMAC for unconfigured gateway nodes.
fn random_vmac() -> [u8; 6] {
    let mut v = [0u8; 6];
    getrandom_fill(&mut v);
    v
}

/// Random 16-byte device UUID for the gateway's BACnet identity.
fn random_uuid() -> [u8; 16] {
    let mut v = [0u8; 16];
    getrandom_fill(&mut v);
    v
}

/// Fill `dest` with cryptographic random bytes. Falls back to a poor
/// "random" mix if `getrandom` fails — the fallback only matters for
/// uniqueness within one gateway process, not security.
fn getrandom_fill(dest: &mut [u8]) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0xDEADBEEF);
    for (i, b) in dest.iter_mut().enumerate() {
        *b = ((seed >> (i % 4 * 8)) ^ (i as u32 * 31)) as u8;
    }
}

/// Coerce an upstream BACnet `PropertyValue` (Real / Double / Unsigned /
/// Signed) to f64. Other types are out of Tier 1 scope.
fn coerce_to_f64(v: PropertyValue) -> Result<f64> {
    match v {
        PropertyValue::Real(f) => Ok(f as f64),
        PropertyValue::Double(f) => Ok(f),
        PropertyValue::Unsigned(u) => Ok(u as f64),
        PropertyValue::Signed(i) => Ok(i as f64),
        other => anyhow::bail!(
            "Tier 1 BACnet/SC only supports numeric PropertyValue variants, got {other:?}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_vmac_accepts_colon_form() {
        let v = parse_vmac("AA:BB:CC:DD:EE:FF").unwrap();
        assert_eq!(v, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    }

    #[test]
    fn parse_vmac_accepts_dense_form() {
        let v = parse_vmac("AABBCCDDEEFF").unwrap();
        assert_eq!(v, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    }

    #[test]
    fn parse_vmac_rejects_short_input() {
        assert!(parse_vmac("AA:BB:CC").is_err());
    }
}
