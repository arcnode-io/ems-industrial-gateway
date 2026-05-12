---
name: ems-protocol-iteration
description: Use when adding a new south-side grid protocol (Modbus TCP / Modbus RTU / SNMP / DNP3 / Redfish / BACnet MS-TP / OCPP) to the ARCNODE EMS stack — covers fixture, gateway client, schema, and e2e in one recipe. Reads/updates `learnings.md` to compound gotchas across protocols.
---

# EMS Protocol Iteration

For each new south-side protocol N, execute the 6 phases below. Read `learnings.md` BEFORE Phase 1 — it captures gotchas from prior rounds that this round must respect.

**Status (from edp-module-assemblies equipment scope):**
- ✅ Done: Modbus TCP, SNMP v2c, Redfish, DNP3 TCP
- ⏳ Pending: BACnet MS-TP (EXT-DC-001, EXT-DC-002 cooling), Modbus RTU (GRD-XFM-001 transformer thermometer)
- ❌ Dead: CANopen — EXT-BESS-002 (CATL) exposes Modbus TCP via MBMU ETH/RJ45; no CAN integration needed

**Announce at start:** "I'm using the ems-protocol-iteration skill to add <PROTOCOL>."

---

## Pre-flight

- [ ] Read `~/.claude/skills/ems-protocol-iteration/learnings.md` end-to-end. Every entry there is a real bullet wound from a prior round.
- [ ] Confirm the gateway Tier 1 stub for Modbus is green (`gitlab.com/arcnode-io/ems-industrial-gateway` main pipeline). New protocols build on its scaffolding (testcontainers, shared Docker network, validated structs).

## Phase 1: Survey + Select Library

- [ ] Query domain MCP (`mcp__domain__rag_search`) with specific terms for the protocol's wire format + key operations (e.g., "SNMP v3 GET PDU + authentication", "DNP3 object groups 30/40 read"). Strong coverage exists for Modbus, SNMPv3, Redfish; DNP3 is thin (Clarke is vector-only); CANopen is absent. If the corpus is silent, use context7 or websearch.
- [ ] Crates.io: identify Rust library candidates. Prefer one with both client + server impls so the fixture and gateway share a library. Examples that worked for Modbus: `rodbus 1.4`. Check `max_stable` version + last-update date via crates.io API.
- [ ] Verify the library's API surface against the fullstack-energy reference at `~/fullstack-energy/{grid-gateway,fixtures}/` — its code is "very bad" per the user but the API call patterns are real.
- [ ] STOP and present the library choice with one-sentence rationale before proceeding.

## Phase 2: Fixture (server-side mock with simulated data)

Path: `~/arcnode/ems-industrial-fixtures/mock-<protocol>-<role>/` (e.g., `mock-snmp-agent`).

**The fixture must produce simulated dynamic data on an interval — not a single canned value.** Integration tests need to observe continuous behavior (values change, stay in plausible ranges, follow a pattern). Canned single-value fixtures only support one-shot e2e tests.

Recipe:
- `src/simulator.rs`: a `Simulator` that mutates an in-memory data map on a tick. For each protocol-natural address (Modbus register / SNMP OID / DNP3 point_index), define a plausible range + behavior (random walk, sawtooth, sinusoid). Use `rand` for variation. Spawn a tokio task in `main.rs` that calls `simulator.tick()` on a configurable interval (default 1s).
- `src/handler.rs`: implement the library's server-handler trait. Reads pull current values from the shared in-memory map (`Arc<Mutex<...>>` or equivalent — the simulator writes, the handler reads).
- `src/main.rs`: tokio entry, build simulator + handler, spawn the simulator tick loop AND the protocol server task, log `<binary> listening` so testcontainers can wait on it.

What integration tests assert against this fixture:
- Value present on the expected channel + correct type
- Value within the plausible range for that template (e.g., kwh_delivered >= 0, power_factor in [-1, 1])
- Value changes over time (sample N times across M seconds, assert not all equal)

What `Cargo.toml` needs: the protocol lib, tokio (with `time` feature for intervals), tracing, tracing-subscriber, rand.
- [ ] `Dockerfile`: 2-stage `rust:1.93-slim` builder → `debian:bookworm-slim`. Stub other workspace members and the root `src/lib.rs` so cargo resolves the workspace cleanly.
- [ ] `.gitlab-ci.yml`: add a `publish-mock-<protocol>-<role>` job pushing to **Harbor** `173.211.12.43:8083/library/<image>` (not GitLab registry). Add `publish` stage if missing.
- [ ] Smoke: `docker build … && docker run` locally to confirm log line + port bind.
- [ ] Push + wait for CI to publish image to Harbor before Phase 5.

## Phase 3: Schema additions

### device-api (TypeScript)

- [ ] `src/templates/template.protocols.schema.ts`: add `<Protocol>Binding` as a `strictObject` discriminated by `protocol: z.literal("<name>")`. Add to the `Binding = z.discriminatedUnion("protocol", [...])` list.
- [ ] `src/asyncapi/spec-extensions.ts`: `buildProtocolSourceMap` already merges device.connection into the per-channel binding via `collectBindings(tpl, device.connection)`. Confirm the new protocol's binding fields don't collide with connection fields (host/port/unit_id).

### gateway (Rust)

- [ ] `src/asyncapi/types.rs`: extend `ProtocolBinding` to cover the new protocol's fields, OR — for >2 protocols — refactor to `enum ProtocolBinding { ModbusTcp(ModbusTcpBinding), Snmp(SnmpBinding), ... }` with `#[serde(tag = "protocol")]`. Each variant gets serde `Deserialize` + validator `Validate` derives.

## Phase 4: Gateway client

Path: `~/arcnode/ems-industrial-gateway/src/<protocol>/`.

- [ ] `src/<protocol>/mod.rs` + `src/<protocol>/client.rs`: wrap the protocol library. Mirror the Modbus pattern:
  - `pub async fn read_<thing>(host, port, addr, ...) -> Result<RawValue>` — the I/O call
  - Pure helpers for `decode_<datatype>(...)` + `apply_scale_offset(...)` (or protocol equivalent)
  - **Retry loop around the read** — first attempt often races with TCP handshake. Modbus pattern: 5 attempts with `500ms * 2^attempt` backoff. Test in CI; reduce attempts if not needed.
- [ ] Unit tests in `src/<protocol>/client_test.rs`: the pure decode helpers only (high silent-wrong-value risk, cheap to test). I/O happens in e2e.
- [ ] `src/app.rs`: branch on protocol type (or dispatch through an enum) to call the right client per measurement.
- [ ] `src/lib.rs`: `pub mod <protocol>;`

## Phase 5: E2E

- [ ] `tests/fixtures/containers.rs`: add `start_mock_<protocol>_<role>()` pulling `173.211.12.43:8083/library/<image>:latest`. NOT on the shared Docker network (gateway reaches it via host port mapping; only postgres/emqx/device-api need the network).
- [ ] `tests/fixtures/seed_dtm.json`: add a device using the new protocol's template. Include `equipment_id`, `vendor`, `model` (required for kind=leaf templates). Connection block uses fixture's host:port (filled in at test time).
- [ ] `tests/integration_test.rs`: extend the existing e2e to also assert the new protocol's measurement publishes to its expected topic + value.
- [ ] Cargo audit ignores: if the new protocol library pulls TLS/DNS transitive deps, add the RUSTSEC IDs to `audit-packages` in `Cargo.toml`.

## Phase 6: Compound learnings

- [ ] After the pipeline lands green, append to `~/.claude/skills/ems-protocol-iteration/learnings.md` under a new `## <Protocol> (YYYY-MM-DD)` section:
  - Library quirks (API surface, retry needs, handshake races)
  - Schema gotchas (data types, optional fields, required combos)
  - Test gotchas (testcontainer wait strategy, port mapping, network alias needs)
  - One-liner: "What I wish I'd known before starting this protocol"
- [ ] Update SKILL.md only if the recipe itself needs revision — not for protocol-specific gotchas (those belong in learnings).

## Anti-patterns (saved learnings)

- **Don't kick-the-can.** If the right shape is cheap now (e.g., enum variant instead of one bloated struct), do it now. Migrations leave artifacts.
- **Don't trust modelina for AsyncAPI 3.0 → Rust.** It works for bare JSON Schema, not for AsyncAPI docs. Hand-roll validated structs.
- **Don't use GitLab Container Registry.** Project convention is Harbor at `173.211.12.43:8083/library/`.
- **Don't put mock fixture on the device-api Docker network.** Gateway reaches the fixture via host port mapping. Only the device-api stack (postgres/emqx/device-api) needs the shared network — device-api's `beta:` cfg block hardcodes `postgres` + `emqx` hostnames.
- **Don't expect the first protocol read to succeed.** Retry with backoff — the library's auto-reconnect typically races with the first call.
