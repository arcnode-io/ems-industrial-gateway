//! Hand-rolled validated structs that mirror device-api's `/asyncapi` shape.
//!
//! Gateway only consumes a narrow slice (x-protocol-source bindings). When the
//! spec shape changes, update these structs — the compiler tells you where to
//! look. `Validate` is used only on the top-level metadata block; per-binding
//! fields are enforced at parse time by serde's typed deserialization.

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
    /// Per-device, per-measurement protocol bindings.
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

/// Per-device, per-measurement protocol binding. Variants discriminated by
/// the `protocol` key in JSON (matches `template.protocols.schema.ts`).
#[derive(Debug, Deserialize)]
#[serde(tag = "protocol")]
pub enum ProtocolBinding {
    /// Modbus TCP binding.
    #[serde(rename = "modbus_tcp")]
    ModbusTcp(ModbusTcpBinding),
    /// SNMP (v2c) binding.
    #[serde(rename = "snmp")]
    Snmp(SnmpBinding),
}

/// Modbus TCP binding fields (template + device.connection merged in
/// device-api's `x-protocol-source` extension).
#[derive(Debug, Deserialize)]
pub struct ModbusTcpBinding {
    /// Target host (IP or DNS) for the protocol connection.
    pub host: String,
    /// TCP port for the protocol connection.
    pub port: u16,
    /// Modbus unit id (slave id). Stored as string in the DTM; parsed to u8
    /// at the Modbus call site.
    pub unit_id: String,
    /// Starting register address.
    pub address: u16,
    /// Linear-scale factor applied to the raw register value.
    pub scale: f64,
    /// Linear offset applied after scaling.
    pub offset: f64,
}

/// SNMP v2c binding fields.
#[derive(Debug, Deserialize)]
pub struct SnmpBinding {
    /// Target host (IP or DNS) for the SNMP agent.
    pub host: String,
    /// UDP port for the SNMP agent (default 161).
    pub port: u16,
    /// Object identifier in dotted-numeric form, e.g. "1.3.6.1.4.1.41999.1.1.0".
    pub oid: String,
}
