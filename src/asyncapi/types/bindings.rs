//! Per-protocol binding field structs — one per `ProtocolBinding` variant.

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
}

/// Synthetic binding: gateway computes a value from cached MQTT inputs via a
/// named operation. No south-side device; the "south" is MQTT itself.
///
/// Topic placeholders in `inputs`:
/// - `{site_id}` — substituted from gateway runtime config at subscribe time.
/// - `{device_id}` — already resolved by `ems-device-api` at AsyncAPI gen time.
#[derive(Debug, Deserialize)]
pub struct SyntheticBinding {
    /// Operation name: `subtract`, `sum`, `mean`, `max`, `min`, `weighted_mean`.
    pub operation: String,
    /// Input topic templates the synthetic task subscribes to and caches.
    /// Present for `subtract`/`sum`/`mean`/`max`/`min`; absent (empty) when
    /// `operation` is `weighted_mean`, which uses `pairs` instead.
    #[serde(default)]
    pub inputs: Vec<String>,
    /// `(topic, weight)` pairs — only present when `operation` is
    /// `weighted_mean`. Device-api resolves the weight (e.g. a child rack's
    /// `capacity_kwh`) at spec-build time; the gateway just reads the named
    /// topics and applies `synthetic::operation::weighted_mean`.
    #[serde(default)]
    pub pairs: Vec<WeightedPair>,
}

/// One `(topic, weight)` entry in a `weighted_mean` synthetic binding.
#[derive(Debug, Clone, Deserialize)]
pub struct WeightedPair {
    /// The MQTT topic to read the value from.
    pub topic: String,
    /// The weight to apply to that topic's value.
    pub weight: f64,
}

/// Command-distribution binding: a `bess_module`-style command splits one
/// setpoint across N children (e.g. `bess_rack` instances) via a max-min
/// fair allocation policy. See `dispatch::allocation`.
#[derive(Debug, Deserialize)]
pub struct DistributeBinding {
    /// Allocation policy name: `equal_split` or `soc_weighted`.
    pub allocation_policy: String,
    /// Fully-resolved children to distribute the setpoint across.
    pub children: Vec<ChildAllocation>,
}

/// One child's identity + static bounds for command distribution.
#[derive(Debug, Clone, Deserialize)]
pub struct ChildAllocation {
    /// The child device's id — resolves its own write binding via the same
    /// (device_id, verb+target) lookup the module's own command used.
    pub device_id: String,
    /// MQTT topic carrying the child's cached `operating_state` reading —
    /// `FAULT`/`OFFLINE` exclude it from allocation.
    pub operating_state_topic: String,
    /// MQTT topic carrying the child's cached `state_of_charge` reading —
    /// only read for the `soc_weighted` policy.
    pub state_of_charge_topic: String,
    /// The child's own static lower bound (e.g. `active_power.bounds.min`).
    pub power_min: f64,
    /// The child's own static upper bound (e.g. `active_power.bounds.max`).
    pub power_max: f64,
}
