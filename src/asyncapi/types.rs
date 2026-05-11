//! Hand-rolled validated structs that mirror device-api's `/asyncapi` shape.
//!
//! Gateway only consumes a narrow slice (x-protocol-source bindings). When the
//! spec shape changes, update these structs — the compiler tells you where to
//! look. `Validate` enforces business rules at parse time.

use serde::Deserialize;
use std::collections::HashMap;
use validator::Validate;

/// Top-level AsyncAPI v3 spec, narrowed to fields the gateway reads.
/// Extra keys in the JSON are ignored by serde's default behavior.
#[derive(Debug, Deserialize, Validate)]
pub struct AsyncApiSpec {
    #[validate(nested)]
    pub info: SpecInfo,
    #[serde(rename = "x-protocol-source")]
    pub x_protocol_source: HashMap<String, HashMap<String, ProtocolBinding>>,
}

/// AsyncAPI info block.
#[derive(Debug, Deserialize, Validate)]
pub struct SpecInfo {
    #[validate(length(min = 1))]
    pub version: String,
}

/// Per-device, per-measurement protocol binding (Tier 1: Modbus only).
#[derive(Debug, Deserialize, Validate)]
pub struct ProtocolBinding {
    #[validate(length(min = 1))]
    pub host: String,
    #[validate(range(min = 1, max = 65535))]
    pub port: u16,
    pub unit_id: u8,
    pub address: u16,
    pub scale: f64,
    pub offset: f64,
}
