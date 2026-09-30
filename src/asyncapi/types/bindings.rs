//! Per-protocol binding field structs — one per `ProtocolBinding` variant.

use crate::modbus::codec::{ModbusDataType, WordOrder};
use serde::Deserialize;

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
    /// Register wire width. Defaults to `Int32` — matches every binding
    /// that predates this field, so a payload omitting it (real devices
    /// before edp-api annotated data_type, or a hand-rolled test stub)
    /// keeps the old 2-register behavior.
    #[serde(default)]
    pub data_type: ModbusDataType,
    /// Multi-register word order. Defaults to `HighLow`.
    #[serde(default)]
    pub word_order: WordOrder,
    /// Modbus function code: 3/4 for a measurement (holding/input
    /// registers), 6/16 for a command (write single/multiple). Absent keeps
    /// the old FC3 read / FC16 write. Checked per entry at spec parse.
    #[serde(default)]
    pub function_code: Option<u8>,
    /// SunSpec `sunssf`: the register holding this value's decimal exponent
    /// (int16), read every poll: value = raw × 10^sf, before scale/offset.
    /// Measurements only; a command carrying one is skipped at spec parse.
    #[serde(default)]
    pub scale_factor_address: Option<u16>,
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
    /// Multiplier from the raw integer to engineering units (e.g. 0.01 for a
    /// MIB that reports current in hundredths of an amp). Absent = 1.0.
    #[serde(default = "unit_scale")]
    pub scale: f64,
}

/// `scale` when a spec omits it (SNMP, Redfish, DNP3).
fn unit_scale() -> f64 {
    1.0
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
    /// Multiplier from the raw reading to the measurement's unit (e.g. 1e6
    /// for a vendor OEM property in MHz where the unit is hertz). Absent = 1.0.
    #[serde(default = "unit_scale")]
    pub scale: f64,
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

/// BACnet/SC (ASHRAE 135-2020 Annex AB) binding fields. The gateway dials
/// the configured `hub_url` (wss://...), authenticates with mTLS using
/// the credentials in `cfg.gateway_credentials`, then sends a
/// ReadProperty over the hub addressed to `device_vmac`.
#[derive(Debug, Deserialize)]
pub struct BacnetScBinding {
    /// `wss://host:port/` URL of the BACnet hub the device is connected to.
    pub hub_url: String,
    /// Destination VMAC (6 bytes) as a colon-separated hex string,
    /// e.g. `"AA:BB:CC:DD:EE:FF"`.
    pub device_vmac: String,
    /// Object type to read; Tier 1 supports `analog_input` only.
    pub object_type: String,
    /// Object instance number on the target device.
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
    /// Optional outstation static variation (audit metadata; gateway uses
    /// default-variation polling when unset).
    #[serde(default)]
    pub variation: Option<u8>,
    /// Multiplier from the raw point value to the measurement's unit (e.g.
    /// 1000 for a relay reporting kV primary where the unit is volts).
    /// Absent = 1.0.
    #[serde(default = "unit_scale")]
    pub scale: f64,
}
