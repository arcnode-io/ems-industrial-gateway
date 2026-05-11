# EMS Industrial Gateway 🌉📡

![](https://img.shields.io/gitlab/pipeline-status/arcnode-io/ems-industrial-gateway?branch=main&logo=gitlab)
![](https://gitlab.com/arcnode-io/ems-industrial-gateway/badges/main/coverage.svg)
![](https://img.shields.io/badge/1.93-gray?logo=rust)

> Rust gateway translating south-side grid protocols to north-side MQTT, driven by the AsyncAPI spec served by ems-device-api.

## Scope (Tier 1)

One device (`meter_01` / `revenue_meter`), one measurement (`kwh_delivered`), one Modbus TCP read → one MQTT publish → exit. End-to-end test validates the AsyncAPI/Modbus/MQTT contract against a real ems-device-api. See [`ems/docs/superpowers/specs/2026-05-10-gateway-stub-contract-validation-design.md`](../ems/docs/superpowers/specs/2026-05-10-gateway-stub-contract-validation-design.md).

## Pre-requisites

- Rust 1.93+
- Docker (for the e2e test)
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

industrial_gateway -> device_api: GET /asyncapi
device_api -> industrial_gateway: AsyncAPI v3 spec\n(channels + x-protocol-source bindings)
industrial_gateway -> modbus_fixture: read holding registers
industrial_gateway -> mqtt_broker: publish FloatSample
```

The gateway fetches `/asyncapi` with exponential backoff (boot-time race when device-api is still warming up) and parses it into validated structs (`AsyncApiSpec`, `ProtocolBinding`) — serde for deserialization, `validator` for business-rule checks at parse time.

## Project Structure

```
src/
├── main.rs              # tokio entry, init tracing, calls app::run
├── lib.rs               # crate library surface (used by integration tests)
├── app.rs               # orchestration: fetch /asyncapi → modbus read → MQTT publish
├── config.rs            # cfg.yml deserialize
├── asyncapi/
│   ├── mod.rs
│   └── types.rs         # validated AsyncApiSpec + ProtocolBinding
├── http/
│   ├── mod.rs
│   └── client.rs        # fetch_asyncapi with exponential backoff
├── modbus/
│   ├── mod.rs
│   ├── client.rs        # rodbus client, decode_int32, scale/offset
│   └── client_test.rs   # decode unit tests
└── mqtt/
    ├── mod.rs
    ├── publisher.rs     # FloatSample publish
    └── subscriber.rs    # system/topology_changed sub (logs only)

tests/
├── integration_test.rs  # e2e: 4 testcontainers + real gateway binary
└── fixtures/
    ├── mod.rs
    ├── containers.rs    # postgres, emqx, device-api, mock-modbus-server
    └── seed_dtm.json    # DTM with revenue_meter wired to fixture
```

## Run locally

```bash
# Boot device-api + emqx + mock-modbus-server separately, then:
cargo run
```

Cfg picks `local:` block from `cfg.yml` by default; `ENV=beta cargo run` switches to `beta:`.

## Run the e2e test

```bash
cargo test --test integration_test
```

Pulls `173.211.12.43:8083/library/{ems-device-api,mock-modbus-server}:latest` from Harbor and brings up the full stack. First run takes ~60s for container boot; subsequent runs are faster.

## What the e2e proves

- `device-api` POST /topology accepts a `revenue_meter` DTM and persists it.
- `device-api` regenerates `/asyncapi` with `x-protocol-source` populated.
- Gateway fetches and validates the spec end-to-end.
- Gateway connects to the Modbus fixture, reads `int32` at register 4000 with `high_low` word order, applies `scale`/`offset`.
- Gateway publishes a `FloatSample` to the canonical topic with the decoded value.
- Test-side MQTT subscriber receives `value: 1_000_000.0` (the fixture's canned register content).
