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
    /// Spec metadata block (carries `version` for cache-keying).
    #[validate(nested)]
    pub info: SpecInfo,
    /// Per-device, per-measurement protocol bindings (the gateway's payload).
    #[serde(rename = "x-protocol-source")]
    pub x_protocol_source: HashMap<String, HashMap<String, ProtocolBinding>>,
}

/// AsyncAPI info block.
#[derive(Debug, Deserialize, Validate)]
pub struct SpecInfo {
    /// Monotonic version assigned by device-api on persist.
    #[validate(length(min = 1))]
    pub version: String,
}

/// Per-device, per-measurement protocol binding (Tier 1: Modbus only).
#[derive(Debug, Deserialize, Validate)]
pub struct ProtocolBinding {
    /// Target host (IP or DNS) for the protocol connection.
    #[validate(length(min = 1))]
    pub host: String,
    /// TCP port for the protocol connection.
    #[validate(range(min = 1, max = 65535))]
    pub port: u16,
    /// Modbus unit id (slave id). String in DTM to allow non-numeric IDs for
    /// non-Modbus protocols; gateway parses to u8 at the Modbus call site.
    pub unit_id: String,
    /// Starting register address.
    pub address: u16,
    /// Linear-scale factor applied to the raw register value.
    pub scale: f64,
    /// Linear offset applied after scaling.
    pub offset: f64,
}
