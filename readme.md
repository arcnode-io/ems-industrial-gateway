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
  rectangle mock_ocpp_station
}

rectangle industrial_gateway {
  rectangle modbus_adapter
  rectangle snmp_adapter
  rectangle ocpp_adapter
}

queue mqtt_broker

mock_modbus_server -- modbus_adapter
mock_snmp_agent -- snmp_adapter
mock_ocpp_station -- ocpp_adapter

modbus_adapter -- mqtt_broker
snmp_adapter -- mqtt_broker
ocpp_adapter -- mqtt_broker

note right of fixtures
  Mock fixtures for testing
  Easily swapped for real devices
end note

note right of mqtt_broker
  Topics include units:
  voltage_volts
  current_amps
  power_watts
end note
```


## Topic Structure

Parseable structure with unit as path segment:

```
sites/{site_id}/devices/{device_id}/measurements/{unit}/{measurement}
sites/{site_id}/devices/{device_id}/commands/{command}
sites/{site_id}/devices/{device_id}/status/{status}
```

Examples:
- `sites/site_001/devices/meter_01/measurements/volts/120.5`
- `sites/site_001/devices/meter_01/measurements/amps/25.3`
- `sites/site_001/devices/bess_01/measurements/percent/85.2`

Where:
- `{unit}` is enum: `volts`, `amps`, `watts`, `percent`, `celsius`, etc.
- `{measurement}` is float value

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
│       ├── bacnet/          # BACnet UDP
│       ├── dnp3/            # DNP3 TCP
│       └── ocpp/            # OCPP WebSocket
└── tests/
    └── integration.rs
```

### Component Responsibilities
- **main.rs**: Application entry point, coordinates adapters
- **device_api_client.rs**: HTTP client to fetch AsyncAPI spec from device API
- **mqtt.rs**: Shared MQTT publisher used by all adapters
- **adapters/**: Protocol-specific implementations that use shared MQTT publisher
