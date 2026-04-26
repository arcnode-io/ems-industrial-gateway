# EMS Industrial Gateway 🌉📡

![](https://img.shields.io/gitlab/pipeline-status/arcnode-io/ems-industrial-gateway?branch=main&logo=gitlab)
![](https://gitlab.com/arcnode-io/ems-industrial-gateway/badges/main/coverage.svg)
![](https://img.shields.io/badge/1.93-gray?logo=rust)

> Simple protocol adapters that publish directly to MQTT with units in topics

## Pre-requisites
- rust 1.93+

## Diagrams

### Deployment
```plantuml
rectangle fixtures #line.dashed {
  rectangle mock_modbus_server
  rectangle mock_snmp_agent
  rectangle mock_redfish_service
}

rectangle industrial_gateway {
  rectangle modbus_adapter
  rectangle snmp_adapter
  rectangle redfish_adapter
}

queue mqtt_broker

modbus_adapter -- mock_modbus_server: modbus\n tcp
snmp_adapter -- mock_snmp_agent: snmp
redfish_adapter -- mock_redfish_service: redfish\n http

modbus_adapter -u- mqtt_broker
snmp_adapter -u- mqtt_broker
redfish_adapter -u- mqtt_broker

note right of fixtures
    All adapters 
    publish
    over mqtt.
end note

```


## Topic Structure

Per [ems/topic_structure_adr.md](../ems/topic_structure_adr.md). Two families, fixed depth per family.

```
sites/{site_id}/devices/{device_id}/measurements/{measurement}/{unit}    # 6 segments
sites/{site_id}/devices/{device_id}/commands/{verb}/{target}/{unit}      # 7 segments
```

Examples — payload is `FloatSample {ts, value}` unless noted:
- `sites/site_001/devices/meter_01/measurements/voltage/volts`           → `{ts, value: 120.5}`
- `sites/site_001/devices/meter_01/measurements/current/amps`            → `{ts, value: 25.3}`
- `sites/site_001/devices/bess_01/measurements/state_of_charge/percent`  → `{ts, value: 85.2}`
- `sites/site_001/devices/bess_01/commands/set/active_power/watts`       → `{ts, value: 5000}`

The gateway is the translation boundary — raw protocol values never reach MQTT:

- **Scaling** — Modbus uint16, DNP3 int32, SNMP Gauge32, etc. are converted to engineering units using `scale`/`offset` from the `x-source` binding in the AsyncAPI spec
- **Enum translation** — for `type: enum` channels, the gateway maps the raw integer to the string label using `register_value` entries from the spec (e.g. `1 → "MANUAL"`). MQTT carries the string; no consumer does int-to-label lookup.

## Device API Integration

Gateway fetches its AsyncAPI spec from device API at startup:

```plantuml
participant industrial_gateway
participant device_api
queue mqtt_broker

industrial_gateway -> device_api: GET /asyncapi
device_api -> industrial_gateway: AsyncAPI v3 spec\n(topics + x-* protocol bindings)
industrial_gateway --> mqtt_broker
```

## Adding/Removing Protocols

### Add Protocol
1. Create folder: `src/adapters/new_protocol/`
2. Update `src/adapters/mod.rs` to include new module

### Remove Protocol  
1. Delete folder: `src/adapters/old_protocol/`
2. Remove from `src/adapters/mod.rs`

## Project Structure
```
├── Cargo.toml
├── src/
│   ├── main.rs              # Application entry point
│   ├── device_api_client.rs     # HTTP client for domain API
│   ├── mqtt.rs              # Shared MQTT publisher
│   └── adapters/
│       ├── mod.rs           # Lists active adapters
│       ├── modbus/          # Modbus TCP
│       ├── canbus/          # CANbus TCP
│       ├── snmp/            # SNMP UDP
│       ├── redfish/         # Redfish HTTPS/JSON
│       └── dnp3/            # DNP3 TCP
└── tests/
    └── integration.rs
```

### Component Responsibilities
- **main.rs**: Application entry point, coordinates adapters
- **device_api_client.rs**: HTTP client to fetch AsyncAPI spec from device API
- **mqtt.rs**: Shared MQTT publisher used by all adapters
- **adapters/**: Protocol-specific implementations that use shared MQTT publisher
