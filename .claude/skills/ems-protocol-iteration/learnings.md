# EMS Protocol Iteration — Compounding Learnings

Append a new section after each protocol lands. Read top-to-bottom before starting a new protocol.

---

## Modbus TCP (2026-05-10)

**Library:** `rodbus 1.4.0` (max stable; `1.5.0-RC1` pre-release exists).

**Library API quirks (mismatches with my first guess):**
- `RetryStrategy` is in `rodbus::` not `rodbus::client::`.
- `RequestParam` is in `rodbus::client::` not `rodbus::`.
- `RetryStrategy` is a trait — use `rodbus::default_retry_strategy()`, not `RetryStrategy::default()`.
- Use `HostAddr::dns(host_string, port)` for the TCP address — not `SocketAddr::from(...)`.
- `AddressRange::try_from(addr, count)` returns `InvalidRange` which does NOT impl `std::error::Error` — can't use `?` to convert to `anyhow::Error`. Map with `.map_err(|e| anyhow::anyhow!("..."))`.

**TCP handshake race:** `channel.enable().await` returns BEFORE the connection actually establishes. First `read_holding_registers` call often errors with "no connection to server". **Retry loop is mandatory** — Modbus uses 5 attempts with `500ms * 2^attempt` backoff.

**Schema gotchas:**
- DTM's `Connection.unit_id` is `z.string().nullish()` (stringly-typed). Gateway must `.parse::<u8>()` at the Modbus call site, not at struct decode.
- DTM's `Connection.port` is `ProvisionedInt = z.union([z.number().int(), z.literal("PROVISIONED_AT_COMMISSIONING")])` — but for tests it's just a number.
- `kind=leaf` templates REQUIRE `equipment_id`, `vendor`, `model`. Missing any → 400 from POST /topology.

**device-api gap that bit us:** `x-protocol-source` only emitted template-level binding fields (`address`, `scale`, etc.). The per-instance `host`/`port`/`unit_id` lived on `devices.<id>.connection`, NOT merged into the AsyncAPI extension. Fixed in `src/asyncapi/spec-extensions.ts` by passing `device.connection` into `collectBindings()` and spreading it. **All future protocols benefit** — they get host/port/unit_id "for free" from this merge.

**Test gotchas:**
- testcontainers `WaitFor::message_on_stdout("mock-modbus-server listening")` works reliably for the fixture.
- device-api log message to wait for: `"Nest application successfully started"`.
- Postgres readiness: `WaitFor::message_on_stderr("database system is ready to accept connections")`.

**Dockerfile gotcha:** The workspace Cargo.toml at `ems-industrial-fixtures/` has both `[workspace]` AND a `[package]` block (for `cargo cmd` plugin metadata). The `[package]` has no targets, so `cargo build` inside the builder image fails unless you create a stub `src/lib.rs` at the workspace root. Stub the other workspace members too (otherwise their `version.workspace = true` resolution breaks).

**Audit ignores added (RUSTSEC):** rodbus pulls `sfio-rustls-config → rustls-webpki 0.102.8` (4 CVEs) and gateway adds `testcontainers → bollard → hyper-rustls → rustls-pemfile/instant/hickory-proto` (5 more). None of those paths execute (no TLS for Modbus TCP; bollard talks unix socket). Ignore IDs: 2025-0055, 2026-0049, 2026-0098, 2026-0099, 2026-0104, 2026-0118, 2026-0119, 2025-0012, 2024-0384, 2025-0134.

**Modelina dead end:** `@asyncapi/modelina` v5.10 generates Rust types from bare JSON Schema, NOT from AsyncAPI 3.0 docs. Hand-rolled validated structs in `src/asyncapi/types.rs` (serde Deserialize + validator Validate). Don't waste time on modelina codegen pipeline for future protocols.

**Registry:** Harbor at `173.211.12.43:8083/library/` — NOT `registry.gitlab.com/...`. CI uses `docker login -u admin -p $HARBOR_PASSWORD`.

**Docker network:** device-api `beta:` cfg block hardcodes `postgresHost: postgres` and `mqttBrokerUrl: mqtt://emqx:1883`. E2E spins up postgres + emqx + device-api on a shared Docker network (`gateway-e2e`) with container names matching. mock-modbus-server is NOT on the network — gateway (on the host) reaches it via mapped port.

**What I wish I'd known before starting Modbus:**
- The `enable()` race. Retry from day 1.
- Harbor URL, not GitLab registry.
- Connection block doesn't auto-merge into x-protocol-source — fixed it during gateway dev, future protocols inherit.
- `cargo audit` will scream about transitive TLS/DNS deps that aren't actually used; build the ignore list once and move on.

**Simulator pattern (now canonical, applied to Modbus):** `mock-modbus-server` uses `src/simulator.rs` with a data-driven sawtooth strategy. Each channel declares its `{addr_high, addr_low, min, max, step}`. A tokio task ticks every `TICK_MS` (default 1000ms), locks rodbus's wrapped handler, and mutates the holding-register `HashMap` in place. Cross-thread sharing is rodbus's own `wrap()` which produces `Arc<Mutex<MeterHandler>>` — handler stays `pub holding: HashMap<u16, u16>` so simulator + handler read/write the same map. Integration tests assert the published value falls in the sawtooth's known range, not exact equality. Reference pattern from `~/fullstack-energy/fixtures/mock-modbus-server/src/simulate.rs` (with `Int32SawtoothSim` instead of slice-based `RangeIncrease`).

---

## SNMP v2c (2026-05-11)

**Libraries:** `csnmp 0.6.0` (client, gateway side) + `rasn 0.28` / `rasn-snmp 0.28` / `rasn-smi 0.28` (server side, hand-rolled UDP agent). No mature Rust crate provides both client and server — different libs per side.

**Library API quirks:**
- `csnmp::ObjectValue` variant for SNMP "Gauge32" is named `Unsigned32(u32)` (SMI's name for it). Don't look for `Gauge32`.
- `rasn_smi::v2::ObjectSyntax` variants are `Simple(SimpleSyntax)` + `ApplicationWide(ApplicationSyntax)`. The crate provides `From` impls for primitive int types — use `ObjectSyntax::from(value_as_i32)` to wrap integer values directly. Don't reach for `SimpleSyntax::Integer(Integer::Primitive(_))` constructors — that path doesn't exist on this version.
- `rasn::types::ObjectIdentifier::new(arcs)` accepts `impl Into<Cow<'static, [u32]>>`. An owned `Vec<u32>` works (passed by move, NOT by reference — `&vec` triggers a lifetime error). Pass `oid_vec` not `&oid_vec`.
- `Pdu.error_status` and `error_index` are plain `u32` — don't `.into()` them from `0i32`; just use `0` (clippy errors on `0u32.into()` as a useless conversion).
- `rasn-snmp` v2 `Pdus::Response` wraps a tuple struct `Response(Pdu)` — construct with `rasn_snmp::v2::Response(pdu_value)`.

**SNMP wire format reminders (RFC 3416):**
- SNMP v2c runs over UDP. Standard agent port: 161.
- A v2c `Message` = community string + version + `Pdus` enum. Decode entire datagram with `rasn::ber::decode::<Message<Pdus>>(bytes)`.
- GetRequest: per-VarBind exact-OID lookup. GetNextRequest: lexicographic next OID in the agent's view.

**Schema gotchas:**
- DTM `SnmpBinding` (`template.protocols.schema.ts`) is minimal: `{ protocol: "snmp", oid: string }`. The agent host/port come from `device.connection`, merged into `x-protocol-source` per the device-api fix from Modbus round.
- Connection block's `unit_id: null` is valid for SNMP (no slave concept). The Modbus-side `unit_id.parse::<u8>()` happens at the Modbus call site only — never touch it in SNMP-only paths.
- Default SNMP community: `"public"` for v2c reads. Hardcoded in gateway client; revisit when per-device community strings are needed.

**Test gotchas:**
- `testcontainers::core::ContainerPort::Udp(161)` to expose UDP. Default `with_exposed_port(161)` is TCP and fails later with "container does not expose port 161/tcp".
- Looking up the mapped UDP port: `container.get_host_port_ipv4(ContainerPort::Udp(161))`. Plain `get_host_port_ipv4(161)` assumes TCP.
- `SocketAddr::parse()` requires an IP literal, NOT a hostname. testcontainers reports a `Host` (often a hostname like `localhost`). Use `tokio::net::lookup_host((host, port))` to resolve. csnmp's `Snmp2cClient::new` wants a real `SocketAddr`.
- Stale containers between CI runs: shared-network containers (postgres, emqx, device-api) use FIXED names so device-api's beta cfg can resolve them by hostname. A killed prior run leaves them named on the daemon; the next run hits `Conflict: container name "/emqx" already in use`. Fix: `before_script` in `.gitlab-ci.yml` runs `docker rm -f postgres emqx device-api 2>/dev/null` + `docker network rm gateway-e2e`.

**Dockerfile gotcha:** Same workspace-root stub trick from Modbus (`src/lib.rs` + stub other members). Updated mock-snmp-agent Dockerfile lists modbus + other peer dirs so `cargo build -p mock-snmp-agent` resolves the workspace cleanly.

**ProtocolBinding enum refactor:** With Modbus + SNMP, gateway's `src/asyncapi/types.rs` is now a `#[serde(tag = "protocol")]` enum: `ProtocolBinding::ModbusTcp(ModbusTcpBinding)` / `Snmp(SnmpBinding)`. Each variant carries its own validated struct. `app.rs` matches on the variant to dispatch to the right client. Future protocols add a variant + extend the match.

**What I wish I'd known before starting SNMP:**
- Use `tokio::net::lookup_host` for hostname → SocketAddr — `SocketAddr::parse` won't.
- ContainerPort::Udp for both `with_exposed_port` AND `get_host_port_ipv4`.
- CI runners can leave stale named containers; `before_script` cleanup is mandatory for network-pinned fixtures.
- The PDU template `~/arcnode/edp-api/device_templates/leaf/pdu.yaml` is the canonical SNMP test template (Server Tech 1718 enterprise). Mirror its OIDs in the fixture rather than inventing.

---

## Redfish (2026-05-12)

**Libraries:** `reqwest 0.12` (gateway client — already a dep) + `axum 0.8.9` (fixture server). Both standard, both Just Work. The simplest protocol round so far.

**Library API quirks:**
- `axum 0.8` `Router::new().route("/path", get(handler))` + `axum::Json<Value>` return type for JSON responses. Pretty boring.
- `serde_json::Value::pointer(&str)` returns `Option<&Value>` per RFC 6901. JSON Pointer paths start with `/`, segments separated by `/`. Drilling `/Temperatures/0/ReadingCelsius` into our Thermal response gives the inlet temp.

**Schema gotchas:**
- DTM `RedfishBinding` (`template.protocols.schema.ts`): `{ protocol: "redfish", uri: string, json_pointer: string|null }`. Connection block adds host/port via the spec-extensions merge — same pattern as Modbus + SNMP.
- `json_pointer` is nullable in the template — gateway treats `None` as "the entire response body is the value." Tier 1 uses pointer everywhere; nullable case is future-proofing.
- Template URIs are relative to `/redfish/v1` (the Redfish spec service root). Gateway prepends `/redfish/v1` when building the full URL. **Don't** put the prefix in the DTM.

**Test gotchas:**
- `start_mock_redfish_service` exposes plain `ContainerPort::Tcp(8443)` (no UDP/special). Default behavior works.
- Test now spins up 6 containers (`postgres`, `emqx`, `device-api`, `mock-modbus-server`, `mock-snmp-agent`, `mock-redfish-service`). The 3 protocol fixtures stay OFF the shared `gateway-e2e` Docker network — gateway reaches them via host port mapping. Only `postgres`/`emqx`/`device-api` need the network (device-api's beta cfg resolves `postgres` + `emqx` by name).
- `network_switch` template's measurements have `poll_rate_hz` between 0.1 and 1 — for Tier 1 hardcoded one-shot reads, the rate is irrelevant. Won't matter until Tier 2's continuous-poll loop.

**Dockerfile gotcha:** Same workspace-stub trick from earlier protocols. The stub list now includes all four siblings (`mock-modbus-server`, `mock-snmp-agent`, `mock-dnp3-outstation`, `mock-canbus-node`). When adding the NEXT fixture, update its Dockerfile's sibling list accordingly.

**ProtocolBinding enum pays off:** Adding Redfish was three changes total to the gateway side:
1. New enum variant `Redfish(RedfishBinding)` in `src/asyncapi/types.rs`
2. New module `src/redfish/{mod,client}.rs` with `read_measurement(b: &RedfishBinding) -> Result<f64>`
3. One `match` arm in `read_value` in `app.rs` + one entry in `TARGETS`

No app.rs surgery, no copy-paste. The trait extraction from the SNMP→Redfish gap proved its worth.

**What I wish I'd known before starting Redfish:**
- Redfish URIs in templates are relative to `/redfish/v1`. Gateway client owns the prefix.
- axum 0.8's `axum::Json<Value>` + `serde_json::json!()` macro = trivial fixture handlers. Don't over-engineer with custom serializers.
- For first-time-only-protocol cases, a "skin" test that just GETs the fixture's URL with `curl` during smoke validation can catch shape mistakes before testcontainers boot.

---

## DNP3 (2026-05-12)

**Library:** `dnp3 1.6.0` from Step Function Inc. Has both `dnp3::master::*` (client) and `dnp3::outstation::*` (server) — fixture and gateway share one crate. Good docs but the API has tendrils across `dnp3::{app,link,master,outstation,tcp}::*` with non-obvious re-exports.

**Module-path gotchas (cost real time):**
- `Variation` is at `dnp3::app::Variation` (re-exported from a private `variations` module). Don't try `dnp3::app::variations::Variation` — that path is private.
- `Flags` is at `dnp3::app::measurement::Flags` (not `dnp3::app::Flags`).
- `EndpointAddress` + `LinkErrorMode` live in `dnp3::link::*` — used by **both** master + outstation.
- `EndpointList` is in `dnp3::tcp::*` (not `dnp3::master::*`).
- `ResponseHeader` is in `dnp3::app::*` (master's `ReadHandler` consumes it, but it's an app-layer type).
- `Classes`, `EventClasses`, `MasterChannelConfig`, `AssociationConfig`, `AssociationHandler`, `AssociationInformation`, `ReadHandler`, `ReadRequest`, `ReadType`, `HeaderInfo` are all in `dnp3::master::*`.
- `EventBufferConfig`, `Add`, `Update`, `UpdateOptions`, `AnalogInputConfig`, `EventClass` (and all the other DB config types) are in `dnp3::outstation::database::*`.
- The methods `db.add(...)` and `db.update(...)` require `use dnp3::outstation::database::{Add, Update};` (trait methods) — the names look like inherent methods but they're traits.

**`spawn_master_tcp_client` signature has 5 args, not 6:**
```rust
spawn_master_tcp_client(
    LinkErrorMode::Close,
    MasterChannelConfig::new(EndpointAddress::try_new(1)?),
    EndpointList::single(endpoint.to_string()),
    ConnectStrategy::default(),
    NullListener::create(),
)
```
There is no `ConnectOptions` parameter. `ConnectStrategy` controls retry/backoff timing.

**`AssociationConfig` has no `quiet()` constructor.** Use `::new(unsol_class_1_2_3, startup_integrity, event_scan_class_1_2_3, auto_time_sync_class)` — match the crate's master example. There's no public field for `decode_level` or `retry_strategy`; tweak those at channel-level config.

**`ReadRequest::one_byte_range(Variation::Group30Var1, start, stop)` for a single AnalogInput.** `start` and `stop` are `u8`, so Tier 1 caps `point_index` at u8 range (255 max). Group 30 Var 1 = 32-bit-with-flag AnalogInput static read.

**Read handler trait — the only method that ever fires on a static-read response is `handle_analog_input`:**
```rust
fn handle_analog_input(
    &mut self,
    _info: HeaderInfo,
    iter: &mut dyn Iterator<Item = (AnalogInput, u16)>,
) {
    for (ai, idx) in iter {
        // ai.value is f64; idx is the point_index
    }
}
```
Tuple order is `(AnalogInput, u16)` not `(u16, AnalogInput)` — the index is second. Easy to write the destructure backwards.

**Outstation ControlHandler is 5 traits bundled:** `ControlSupport<Group12Var1>` + `ControlSupport<Group41Var1>` + `<Group41Var2>` + `<Group41Var3>` + `<Group41Var4>`, plus the base `ControlHandler` trait. Even a read-only outstation has to satisfy all 5 (the master can theoretically send any of them). Stamp them out with a `macro_rules! reject_control` that emits `select`/`operate` returning `CommandStatus::NotSupported`. Add a doc comment on the macro to satisfy `clippy::missing_docs_in_private_items`.

**Outstation server boot:**
```rust
let server = Server::new_tcp_server(LinkErrorMode::Close, addr);
let outstation = server.add_outstation(/* config */ ...);
outstation.transaction(|db| db.add(point_index, Some(EventClass::Class1), AnalogInputConfig::default()));
server.bind().await?; // returns ServerHandle; spawn the await in a task or it blocks
```
`server.bind()` is the listening point — that's the wait-strategy log line target.

**Schema gotchas:**
- DTM `Dnp3TcpBinding` (gateway side): `{ protocol: "dnp3_tcp", host, port, point_index: u16, point_type: String }`. `point_type` exists so we can branch on `analog_input` vs `binary_input` later; Tier 1 only handles `analog_input`. (Connection.host/port are merged in by spec-extensions just like other protocols.)
- The DTM template `protective_relay` uses `binding.protocol = "dnp3_tcp"` to match the discriminator.
- `unit_id` in connection block is `null` for DNP3 (link-layer addresses are in the gateway client constants, not per-device).

**Test gotchas:**
- `start_mock_dnp3_outstation` exposes `ContainerPort::Tcp(20000)`. Standard pattern.
- Test now spins up 7 containers (postgres, emqx, device-api, mock-modbus-server, mock-snmp-agent, mock-redfish-service, mock-dnp3-outstation). DNS network membership unchanged — only the 3 stack containers (postgres/emqx/device-api) are on `gateway-e2e`.
- Sawtooth range for `phase_a_current` is [100, 200] amps; assertion uses `(100.0..=200.0).contains(amps)`.

**Retry/handshake:**
- DNP3 TCP master does its own integrity poll on association add; before the gateway's `association.read(...)` lands, the startup poll has to complete. In practice on a fresh outstation this is fast enough that the first attempt succeeds — but kept the 5-attempt `500ms * 2^n` retry from the other protocols for the same reasons.
- One unexpected: the master's `enable()` must be called AFTER `add_association`, not before. Otherwise the association sits orphaned and `read()` hangs.

**ProtocolBinding enum still paying off:** Adding DNP3 was exactly 3 changes on the gateway side (same as Redfish): new enum variant, new module `src/dnp3/{mod,client}.rs`, one `match` arm + one `TARGETS` entry in `app.rs`. No surgery.

**What I wish I'd known before starting DNP3:**
- The `dnp3` crate's API surface is deep across many modules. Before writing any client code, read `master/examples/master.rs` AND `outstation/examples/outstation.rs` from the crate — they show the imports needed and the wiring shape. Saves an hour of "unresolved import" whack-a-mole.
- `ControlHandler` is unavoidable on the outstation side even for read-only mocks. Plan to write the `reject_control!` macro on day one.
- `Variation` is publicly at `dnp3::app::Variation`, not `dnp3::master::Variation`. Look for re-exports before reaching for absolute private paths.
