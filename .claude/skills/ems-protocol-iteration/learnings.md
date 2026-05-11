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
