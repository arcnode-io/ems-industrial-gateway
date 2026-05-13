# EMS Industrial Gateway 🌉📡

![](https://img.shields.io/gitlab/pipeline-status/arcnode-io/ems-industrial-gateway?branch=main&logo=gitlab)
![](https://gitlab.com/arcnode-io/ems-industrial-gateway/badges/main/coverage.svg)
![](https://img.shields.io/badge/1.93-gray?logo=rust)

> Rust gateway translating south-side grid protocols to north-side MQTT, driven by the AsyncAPI spec served by ems-device-api.

## Scope

DTM-driven continuous gateway. On boot: connects to MQTT, subscribes to
`system/topology_changed`, fetches `/asyncapi` from ems-device-api, then spawns
one tokio task per `(device, measurement)` channel. Each task owns its own
`interval(1/poll_rate_hz)` ticker, calls into the matching protocol client,
and publishes a `FloatSample` to the channel's canonical MQTT topic. A
topology beacon triggers a full task-set restart against the freshly-fetched
spec. SIGINT or SIGTERM cancels the root token, drains tasks, disconnects
cleanly.

Five south-side protocols ship:

| Protocol | Library | Notes |
|---|---|---|
| Modbus TCP | rodbus | Devices that natively speak Modbus RTU are covered when fronted by a serial→Ethernet bridge (Moxa NPort etc.) |
| SNMP v2c | csnmp | UDP, public community by default |
| Redfish | reqwest | HTTP/HTTPS, JSON Pointer extraction from response body |
| DNP3 TCP | dnp3 | Single-point ReadProperty on AnalogInput |
| BACnet/IP | bacnet-rs | UDP 47808, single ReadProperty; devices behind a BACnet router (Loytec, Easy/IO, ABB) cover MS-TP transparently |

## Pre-requisites

- Rust 1.93+
- Docker (for the integration test)
- Harbor login for `173.211.12.43:8083` (image pulls)

## Topic Structure

Per [ems/topic_structure_adr.md](../ems/topic_structure_adr.md). Two families, fixed depth per family.

```
sites/{site_id}/devices/{device_id}/measurements/{measurement}/{unit}    # 6 segments
sites/{site_id}/devices/{device_id}/commands/{verb}/{target}/{unit}      # 7 segments
```

Payload is `FloatSample {ts, value}` for float measurements. The gateway is the translation boundary — raw protocol values never reach MQTT:

- **Scaling** — Modbus uint16, DNP3 int32, SNMP Gauge32, etc. are converted to engineering units using `scale`/`offset` from the `x-protocol-source` binding in the AsyncAPI spec
- **Enum translation** — for `type: enum` channels, the gateway maps the raw integer to the string label using `register_value` entries from the spec

## Device API Integration

```plantuml
participant industrial_gateway
participant device_api
queue mqtt_broker

industrial_gateway -> mqtt_broker: subscribe system/topology_changed
industrial_gateway -> device_api: GET /asyncapi (initial)
device_api -> industrial_gateway: AsyncAPI v3 spec\n(x-protocol-source: binding + unit + poll_rate_hz)

loop per-measurement task
  industrial_gateway -> south_side_device: protocol read
  industrial_gateway -> mqtt_broker: publish FloatSample
end

mqtt_broker -> industrial_gateway: system/topology_changed beacon
industrial_gateway -> device_api: GET /asyncapi (refresh)
industrial_gateway -> industrial_gateway: cancel + respawn task set
```

The gateway fetches `/asyncapi` with exponential backoff (boot-time race when device-api is still warming up) and parses it into validated structs (`AsyncApiSpec`, `ProtocolSource`, `ProtocolBinding`) — serde for deserialization, `validator` for business-rule checks at parse time. `x-protocol-source` carries the per-channel binding, device connection (`host`/`port`/`unit_id`), `unit`, and `poll_rate_hz` in one entry.

Per-task poll-rate normalization: spec value is clamped to `[0.01, 10.0]` Hz; a missing `poll_rate_hz` defaults to `1.0` Hz.

## Project Structure

```
src/
├── main.rs              # tokio entry, init tracing, wire SIGINT/SIGTERM → cancel
├── lib.rs               # crate library surface (used by integration tests)
├── app.rs               # orchestration: subscribe → fetch → spawn per-channel tasks → reconcile on beacon
├── config.rs            # cfg.yml deserialize
├── asyncapi/
│   ├── mod.rs
│   └── types.rs         # validated AsyncApiSpec + ProtocolSource + ProtocolBinding
├── http/
│   ├── mod.rs
│   └── client.rs        # fetch_asyncapi with exponential backoff
├── bacnet/
│   ├── mod.rs
│   └── client.rs        # bacnet-rs UDP master, single ReadProperty
├── dnp3/
│   ├── mod.rs
│   └── client.rs        # dnp3 TCP master, AnalogInput read
├── modbus/
│   ├── mod.rs
│   ├── client.rs        # rodbus client, decode_int32, scale/offset
│   └── client_test.rs   # decode unit tests
├── redfish/
│   ├── mod.rs
│   └── client.rs        # reqwest GET, JSON Pointer extraction
├── snmp/
│   ├── mod.rs
│   └── client.rs        # csnmp v2c GET
└── mqtt/
    ├── mod.rs
    ├── publisher.rs     # FloatSample publish
    └── subscriber.rs    # system/topology_changed → watch::Receiver

tests/
├── integration_test.rs  # 8 testcontainers + real gateway binary, asserts continuous behavior
└── fixtures/
    ├── mod.rs
    ├── containers.rs    # postgres, emqx, device-api, mock-{modbus,snmp,redfish,dnp3,bacnet}
    └── seed_dtm.json    # DTM with one device per protocol wired to its fixture
```

## Run locally

```bash
# Boot device-api + emqx + the protocol fixtures separately, then:
cargo run
```

Cfg picks `local:` block from `cfg.yml` by default; `ENV=beta cargo run` switches to `beta:`. Gateway runs until SIGINT (Ctrl-C) or SIGTERM.

## Run the integration test

```bash
cargo test --test integration_test
```

Pulls `173.211.12.43:8083/library/{ems-device-api,mock-modbus-server,mock-snmp-agent,mock-redfish-service,mock-dnp3-outstation,mock-bacnet-device}:latest` from Harbor and brings up the full 8-container stack. First run takes ~60s for container boot; subsequent runs are faster.

## What the integration test proves

- `device-api` POST /topology accepts a multi-device DTM (revenue meter, PDU, network switch, protective relay, dry cooler) and persists it.
- `device-api` regenerates `/asyncapi` with `x-protocol-source` populated for every measurement (binding + connection + unit + poll_rate_hz).
- Gateway fetches and validates the spec end-to-end, walks `x-protocol-source`, and spawns one tokio task per channel.
- Each protocol client reads its fixture (Modbus, SNMP, Redfish, DNP3, BACnet/IP) and publishes a `FloatSample` to the canonical MQTT topic at the rate the spec declares.
- Test-side MQTT subscriber collects 3 publishes per topic and asserts (a) the first value lands inside the fixture's expected sawtooth range and (b) at least 2 distinct values were seen — proving the per-channel ticker actually advances between reads.
- Test cancels the gateway via its `CancellationToken`; the gateway drains in-flight ticks, disconnects MQTT cleanly, and the join handle returns `Ok(())`.
