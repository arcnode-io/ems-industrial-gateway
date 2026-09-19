//! Hand-rolled validated structs that mirror device-api's `/asyncapi` shape.
//!
//! Gateway consumes two sibling maps: x-protocol-source (measurements, keyed
//! by channel name, drives the poll loop) and x-command-source (commands,
//! keyed by channel name but resolved by their own verb+target fields — a
//! command topic never carries the template's channel name, only
//! `commands/{verb}/{target}/{unit}`). When the spec shape changes, update
//! these structs — the compiler tells you where to look. `Validate` is used
//! only on the top-level metadata block; per-binding fields are enforced at
//! parse time by serde's typed deserialization.

mod bindings;

pub use bindings::{
    BacnetIpBinding, BacnetScBinding, ChildAllocation, DistributeBinding, Dnp3TcpBinding,
    ModbusTcpBinding, RedfishBinding, SnmpBinding, SyntheticBinding, WeightedPair,
};

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
    /// Per-device, per-command protocol bindings + verb/target identity.
    /// Empty if the spec predates the command/measurement split (default =
    /// no dispatchable commands).
    #[serde(rename = "x-command-source", default)]
    pub x_command_source: HashMap<String, HashMap<String, CommandSource>>,
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

/// One x-command-source entry: a protocol binding plus the identity a
/// `commands/{verb}/{target}/{unit}` topic carries — `verb`/`target` are
/// real fields here because the topic never carries the template's own
/// command name, only these two.
#[derive(Debug, Deserialize)]
pub struct CommandSource {
    /// The command verb (e.g. `set`, `enable`).
    pub verb: String,
    /// The command target within the device (e.g. `active_power`).
    pub target: String,
    /// Engineering unit terminal segment for the MQTT topic.
    pub unit: String,
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
    /// BACnet/SC (Secure Connect) binding — ASHRAE 135-2020 Annex AB.
    /// Standards-compliant secure variant: WebSocket+TLS+mTLS to a
    /// hub, hub forwards to the destination device by VMAC.
    #[serde(rename = "bacnet_sc")]
    BacnetSc(BacnetScBinding),
    /// Synthetic: gateway-computed pure function of cached MQTT inputs.
    /// No south-side protocol; produces an MQTT publish from upstream MQTT
    /// subscriptions. See `src/synthetic/`.
    #[serde(rename = "synthetic")]
    Synthetic(SyntheticBinding),
    /// Distribute: command-only, splits one setpoint across N children via
    /// a max-min fair allocation policy. See `dispatch::allocation`.
    #[serde(rename = "distribute")]
    Distribute(DistributeBinding),
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
            "operation": "subtract",
            "inputs": [
                "sites/{site_id}/devices/operating_envelope/measurements/import_limit/watts",
                "sites/{site_id}/devices/bess_module_1/measurements/active_power/watts"
            ]
        }"#;
        // Act
        let src: ProtocolSource = serde_json::from_str(json).unwrap();
        // Assert — variant + operation + inputs[] survive deserialization
        let ProtocolBinding::Synthetic(b) = src.binding else {
            panic!("expected Synthetic variant");
        };
        assert_eq!(b.operation, "subtract");
        assert_eq!(b.inputs.len(), 2);
        assert!(b.inputs[1].contains("bess_module_1"));
    }

    #[test]
    fn deserialize_command_source_carries_verb_and_target() {
        // Arrange — same shape device-api emits in x-command-source for
        // bess_rack's set_active_power command.
        let json = r#"{
            "verb": "set",
            "target": "active_power",
            "unit": "watts",
            "protocol": "modbus_tcp",
            "host": "10.0.0.7",
            "port": 502,
            "unit_id": "1",
            "address": 50,
            "scale": 1.0,
            "offset": 0.0
        }"#;
        // Act
        let src: CommandSource = serde_json::from_str(json).unwrap();
        // Assert
        assert_eq!(src.verb, "set");
        assert_eq!(src.target, "active_power");
        let ProtocolBinding::ModbusTcp(b) = src.binding else {
            panic!("expected ModbusTcp variant");
        };
        assert_eq!(b.address, 50);
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

    #[test]
    fn deserialize_weighted_mean_synthetic_binding_carries_pairs() {
        // Arrange — bess_module's state_of_charge rollup: capacity-weighted
        // mean over its racks, no `inputs` at all.
        let json = r#"{
            "unit": "percent",
            "poll_rate_hz": 1.0,
            "protocol": "synthetic",
            "operation": "weighted_mean",
            "pairs": [
                { "topic": "sites/{site_id}/devices/bess_rack_1/measurements/state_of_charge/percent", "weight": 2000.0 },
                { "topic": "sites/{site_id}/devices/bess_rack_2/measurements/state_of_charge/percent", "weight": 1000.0 }
            ]
        }"#;
        // Act
        let src: ProtocolSource = serde_json::from_str(json).unwrap();
        // Assert
        let ProtocolBinding::Synthetic(b) = src.binding else {
            panic!("expected Synthetic variant");
        };
        assert_eq!(b.operation, "weighted_mean");
        assert!(b.inputs.is_empty());
        assert_eq!(b.pairs.len(), 2);
        assert!((b.pairs[0].weight - 2000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn deserialize_distribute_binding_carries_children() {
        // Arrange — bess_module's set_active_power distribution across 2 racks.
        let json = r#"{
            "verb": "set",
            "target": "active_power",
            "unit": "watts",
            "protocol": "distribute",
            "allocation_policy": "soc_weighted",
            "children": [
                {
                    "device_id": "bess_rack_1",
                    "operating_state_topic": "sites/{site_id}/devices/bess_rack_1/measurements/operating_state/none",
                    "state_of_charge_topic": "sites/{site_id}/devices/bess_rack_1/measurements/state_of_charge/percent",
                    "power_min": -4000000.0,
                    "power_max": 4000000.0
                }
            ]
        }"#;
        // Act
        let src: CommandSource = serde_json::from_str(json).unwrap();
        // Assert
        let ProtocolBinding::Distribute(b) = src.binding else {
            panic!("expected Distribute variant");
        };
        assert_eq!(b.allocation_policy, "soc_weighted");
        assert_eq!(b.children.len(), 1);
        assert_eq!(b.children[0].device_id, "bess_rack_1");
        assert!((b.children[0].power_max - 4_000_000.0).abs() < f64::EPSILON);
    }
}
