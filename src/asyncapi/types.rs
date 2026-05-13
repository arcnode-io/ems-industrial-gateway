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
    /// Per-device, per-measurement protocol bindings + channel meta.
    #[serde(rename = "x-protocol-source")]
    pub x_protocol_source: HashMap<String, HashMap<String, ProtocolSource>>,
}

/// One x-protocol-source entry: a protocol binding plus the channel-level
/// meta the gateway needs to drive the loop (`unit` for MQTT topic suffix,
/// `poll_rate_hz` for tick cadence).
#[derive(Debug, Deserialize)]
pub struct ProtocolSource {
    /// Engineering unit terminal segment for the MQTT topic.
    pub unit: String,
    /// Poll cadence per measurement; `None` means the DTM author omitted it
    /// and the gateway should apply its default (see `app.rs`).
    pub poll_rate_hz: Option<f64>,
    /// The protocol binding itself; variant discriminated by `protocol`.
    #[serde(flatten)]
    pub binding: ProtocolBinding,
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
    /// Redfish (HTTP+JSON) binding.
    #[serde(rename = "redfish")]
    Redfish(RedfishBinding),
    /// DNP3 over TCP binding.
    #[serde(rename = "dnp3_tcp")]
    Dnp3Tcp(Dnp3TcpBinding),
    /// BACnet/IP binding. Used for devices fronted by a BACnet/IP↔MS-TP
    /// router; the on-device protocol may be MS-TP but the gateway only
    /// sees BACnet/IP UDP.
    #[serde(rename = "bacnet_ip")]
    BacnetIp(BacnetIpBinding),
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

/// Redfish (HTTP+JSON, DSP0266) binding fields.
#[derive(Debug, Deserialize)]
pub struct RedfishBinding {
    /// Target host (IP or DNS) for the Redfish service.
    pub host: String,
    /// HTTP(S) port for the Redfish service (default 8443 or 443).
    pub port: u16,
    /// Resource URI relative to the service root, e.g. "/Chassis/SW1/Thermal".
    /// Gateway prepends `/redfish/v1`.
    pub uri: String,
    /// JSON Pointer (RFC 6901) into the response body, e.g.
    /// "/Temperatures/0/ReadingCelsius". Null means the response IS the value.
    pub json_pointer: Option<String>,
}

/// BACnet/IP (ASHRAE 135 Annex J) binding fields.
#[derive(Debug, Deserialize)]
pub struct BacnetIpBinding {
    /// Target host (IP or DNS) for the BACnet/IP endpoint (router or device).
    pub host: String,
    /// UDP port (default 47808 per Annex J).
    pub port: u16,
    /// Device instance number on the target.
    pub device_instance: u32,
    /// Object type to read; Tier 1 supports `analog_input` only.
    pub object_type: String,
    /// Object instance number on the target.
    pub object_instance: u32,
    /// Property to read; Tier 1 supports `present_value` only.
    pub property_id: String,
}

/// DNP3 TCP binding fields.
#[derive(Debug, Deserialize)]
pub struct Dnp3TcpBinding {
    /// Target host (IP or DNS) for the outstation.
    pub host: String,
    /// TCP port (default 20000).
    pub port: u16,
    /// DNP3 point index on the outstation.
    pub point_index: u16,
    /// Point object class: `analog_input`, `binary_input`, `counter`, etc.
    /// Tier 1 only reads `analog_input`.
    pub point_type: String,
}
