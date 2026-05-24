//! Hand-rolled validated structs that mirror device-api's `/asyncapi` shape.
//!
//! Gateway only consumes a narrow slice (x-protocol-source bindings). When the
//! spec shape changes, update these structs — the compiler tells you where to
//! look. `Validate` is used only on the top-level metadata block; per-binding
//! fields are enforced at parse time by serde's typed deserialization.

use crate::asyncapi::trust::DeviceTrust;
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
    /// Per-device mutual-auth trust material (pinned cert / USM creds /
    /// `none`). Keyed by device_id, parallel to `x-protocol-source`. Empty if
    /// the spec was emitted by a pre-trust device-api (default = no trust).
    #[serde(rename = "x-device-trust", default)]
    pub x_device_trust: HashMap<String, DeviceTrust>,
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
    /// Synthetic: gateway-computed pure function of cached MQTT inputs.
    /// No south-side protocol; produces an MQTT publish from upstream MQTT
    /// subscriptions. See `src/synthetic/`.
    #[serde(rename = "synthetic")]
    Synthetic(SyntheticBinding),
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
    /// Optional outstation static variation (audit metadata; gateway uses
    /// default-variation polling when unset).
    #[serde(default)]
    pub variation: Option<u8>,
}

/// Synthetic binding: gateway computes a value from cached MQTT inputs via a
/// named formula. No south-side device; the "south" is MQTT itself.
///
/// Topic placeholders in `inputs`:
/// - `{site_id}` — substituted from gateway runtime config at subscribe time.
/// - `{device_id}` — already resolved by `ems-device-api` at AsyncAPI gen time.
#[derive(Debug, Deserialize)]
pub struct SyntheticBinding {
    /// Formula name: `subtract`, `sum`, `mean`, `max`, `min`.
    pub formula: String,
    /// Input topic templates the synthetic task subscribes to and caches.
    pub inputs: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_synthetic_binding_from_x_protocol_source_json() {
        // Arrange — same shape device-api emits in x-protocol-source for a
        // bess_module headroom channel (synthetic + publisher=gateway).
        let json = r#"{
            "unit": "watts",
            "poll_rate_hz": 1.0,
            "protocol": "synthetic",
            "formula": "subtract",
            "inputs": [
                "sites/{site_id}/devices/operating_envelope/measurements/import_limit/watts",
                "sites/{site_id}/devices/bess_module_1/measurements/active_power/watts"
            ]
        }"#;
        // Act
        let src: ProtocolSource = serde_json::from_str(json).unwrap();
        // Assert — variant + formula + inputs[] survive deserialization
        let ProtocolBinding::Synthetic(b) = src.binding else {
            panic!("expected Synthetic variant");
        };
        assert_eq!(b.formula, "subtract");
        assert_eq!(b.inputs.len(), 2);
        assert!(b.inputs[1].contains("bess_module_1"));
    }

    #[test]
    fn deserialize_dnp3_binding_accepts_optional_variation() {
        // Arrange — Dnp3 binding with variation set (Group 30 Var 5 = float)
        let json = r#"{
            "unit": "amps",
            "poll_rate_hz": 1.0,
            "protocol": "dnp3_tcp",
            "host": "10.0.0.7",
            "port": 20000,
            "point_index": 10,
            "point_type": "analog_input",
            "variation": 5
        }"#;
        // Act
        let src: ProtocolSource = serde_json::from_str(json).unwrap();
        // Assert
        let ProtocolBinding::Dnp3Tcp(b) = src.binding else {
            panic!("expected Dnp3Tcp variant");
        };
        assert_eq!(b.variation, Some(5));
        assert_eq!(b.point_index, 10);
    }

    #[test]
    fn asyncapi_spec_carries_x_device_trust_block() {
        // Arrange — minimal spec with one device's trust block alongside its
        // protocol source. Device-api emits both blocks side-by-side.
        let json = r#"{
            "info": { "version": "v1" },
            "x-protocol-source": {},
            "x-device-trust": {
                "meter_01": {
                    "trust_mode": "tls_mutual",
                    "subject_name": "meter-01.acme-site.local"
                }
            }
        }"#;
        // Act
        let spec: AsyncApiSpec = serde_json::from_str(json).unwrap();
        // Assert — trust entry keyed by device_id, TlsMutual variant
        let trust = spec
            .x_device_trust
            .get("meter_01")
            .expect("trust for meter_01");
        let crate::asyncapi::trust::DeviceTrust::TlsMutual { subject_name } = trust else {
            panic!("expected TlsMutual variant");
        };
        assert_eq!(subject_name, "meter-01.acme-site.local");
    }

    #[test]
    fn deserialize_dnp3_binding_variation_defaults_to_none() {
        // Arrange — older spec without `variation` field
        let json = r#"{
            "unit": "amps",
            "poll_rate_hz": 1.0,
            "protocol": "dnp3_tcp",
            "host": "10.0.0.7",
            "port": 20000,
            "point_index": 10,
            "point_type": "analog_input"
        }"#;
        // Act
        let src: ProtocolSource = serde_json::from_str(json).unwrap();
        // Assert — variation falls back to None (default)
        let ProtocolBinding::Dnp3Tcp(b) = src.binding else {
            panic!("expected Dnp3Tcp variant");
        };
        assert_eq!(b.variation, None);
    }
}
