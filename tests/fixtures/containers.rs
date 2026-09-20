//! Testcontainer helpers for the gateway e2e test.
//!
//! Postgres / hivemq / device-api join a per-test Docker network so device-api
//! resolves `postgres` and `hivemq` hostnames per its `beta:` cfg block.
//! mock-modbus-server doesn't need the network — the gateway (running on the
//! host) reaches it via the testcontainer's mapped port.
//!
//! Network name AND container name must both be unique per test run: Docker
//! container names are unique daemon-wide (not just per-network), so two
//! concurrently-running test binaries both asking for a container literally
//! named "postgres" collide with a 409 even on separate networks. Each
//! caller generates one `unique_network()` value and threads it through
//! every container in that test's own trio; the fixed hostname device-api
//! actually resolves comes from `.with_hostname(...)`, not the container's
//! (now-unique) `--name`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

/// Generous container-startup ceiling. Default in testcontainers-rs is 60s,
/// which is tight when a CI runner is cold-pulling 7 images in parallel for
/// the big 5-protocol integration test. 180s gives slack without masking
/// genuine startup bugs (those usually fail in <10s).
const STARTUP_TIMEOUT: Duration = Duration::from_secs(180);

/// A Docker network name unique to this process and call — safe to reuse
/// across the postgres/hivemq/device-api trio of a single test, and
/// guaranteed not to collide with any other concurrently-running test
/// binary (or concurrent test within this one).
pub fn unique_network() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("gateway-e2e-{}-{n}", std::process::id())
}

/// Spin up Postgres on `network`, resolvable there as `postgres`.
pub async fn start_postgres(network: &str) -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("postgres", "15")
        .with_exposed_port(ContainerPort::Tcp(5432))
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_PASSWORD", "test")
        .with_network(network)
        .with_hostname("postgres")
        .with_startup_timeout(STARTUP_TIMEOUT)
        .start()
        .await?;
    Ok(c)
}

/// Spin up hivemq on `network`, resolvable there as `hivemq`.
///
/// Uses `with_mapped_port(0, ...)` (single OS-assigned host binding)
/// rather than `with_exposed_port` because the latter triggers
/// testcontainers-rs's `publish_all_ports = true` branch — docker `-P`
/// mode publishes EVERY image-EXPOSE port (HiveMQ exposes 1883/8000/
/// 8083/8443/8883), multiplying collision odds on a busy CI runner
/// (e.g. Harbor on 8083).
pub async fn start_hivemq(network: &str) -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("hivemq/hivemq-ce", "latest")
        .with_wait_for(WaitFor::message_on_stdout(
            "Started TCP Listener on address 0.0.0.0 and on port 1883.",
        ))
        .with_mapped_port(0, ContainerPort::Tcp(1883))
        .with_network(network)
        .with_hostname("hivemq")
        .with_startup_timeout(STARTUP_TIMEOUT)
        .start()
        .await?;
    Ok(c)
}

/// Spin up the platform's custom ems-hivemq image (HiveMQ CE + File RBAC
/// extension, Allow-All stripped). Caller mounts `credentials.xml` at the
/// extension's expected path via `--volume`. Used by the broker-auth test
/// to exercise the gateway's authenticated connect + ACL-enforced topics.
pub async fn start_ems_hivemq_with_credentials(
    credentials_path: &std::path::Path,
) -> anyhow::Result<ContainerAsync<GenericImage>> {
    use testcontainers::core::Mount;
    let abs = credentials_path
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("canonicalize credentials: {e}"))?;
    let abs_str = abs
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-utf8 path"))?;
    let c = GenericImage::new("public.ecr.aws/y1d2j6a8/ems-hivemq", "latest")
        .with_wait_for(WaitFor::message_on_stdout(
            "Started TCP Listener on address 0.0.0.0 and on port 1883.",
        ))
        .with_mapped_port(0, ContainerPort::Tcp(1883))
        .with_mount(Mount::bind_mount(
            abs_str,
            "/opt/hivemq/extensions/hivemq-file-rbac-extension/conf/credentials.xml",
        ))
        .with_startup_timeout(STARTUP_TIMEOUT)
        .start()
        .await?;
    Ok(c)
}

/// Spin up mock-modbus-server. Not on the shared network — gateway reaches it
/// from the host via mapped port. Waits for the CONTROL surface log line
/// (fires after the modbus listener line, so both are up) — requires the
/// post-control-surface image (fixtures f77bf48+); older images log
/// "mock-modbus-server listening" and predate PUT /registers entirely.
pub async fn start_mock_modbus_server() -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("public.ecr.aws/y1d2j6a8/mock-modbus-server", "latest")
        .with_exposed_port(ContainerPort::Tcp(502))
        .with_exposed_port(ContainerPort::Tcp(8080))
        .with_wait_for(WaitFor::message_on_stdout(
            "mock-modbus-server control listening",
        ))
        .with_startup_timeout(STARTUP_TIMEOUT)
        .start()
        .await?;
    Ok(c)
}

/// Spin up mock-modbus-server with function-code-16 writes enabled
/// (`MODBUS_WRITABLE=1`). Same port/wait shape as `start_mock_modbus_server`
/// — only write authorization differs; requires the post-writable-mode image.
pub async fn start_mock_modbus_server_writable() -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("public.ecr.aws/y1d2j6a8/mock-modbus-server", "latest")
        .with_exposed_port(ContainerPort::Tcp(502))
        .with_exposed_port(ContainerPort::Tcp(8080))
        .with_wait_for(WaitFor::message_on_stdout(
            "mock-modbus-server control listening",
        ))
        .with_env_var("MODBUS_WRITABLE", "1")
        .with_startup_timeout(STARTUP_TIMEOUT)
        .start()
        .await?;
    Ok(c)
}

/// Spin up mock-snmp-agent. UDP 161 mapped; gateway reaches it via host port.
pub async fn start_mock_snmp_agent() -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("public.ecr.aws/y1d2j6a8/mock-snmp-agent", "latest")
        .with_exposed_port(ContainerPort::Udp(161))
        .with_wait_for(WaitFor::message_on_stdout("mock-snmp-agent listening"))
        .with_startup_timeout(STARTUP_TIMEOUT)
        .start()
        .await?;
    Ok(c)
}

/// Spin up mock-snmp-agent with SNMPv3 USM enabled (authPriv, SHA-256/AES-128).
/// Passes the agent the security name + passphrases via env vars; the agent
/// derives + caches the localized keys at startup so per-message handling
/// is just a HashMap lookup.
pub async fn start_mock_snmp_agent_v3(
    security_name: &str,
    auth_pass: &str,
    priv_pass: &str,
) -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("public.ecr.aws/y1d2j6a8/mock-snmp-agent", "latest")
        .with_exposed_port(ContainerPort::Udp(161))
        .with_wait_for(WaitFor::message_on_stdout("SNMPv3 USM enabled"))
        .with_env_var("SNMP_V3_USER", security_name)
        .with_env_var("SNMP_V3_AUTH_PASS", auth_pass)
        .with_env_var("SNMP_V3_PRIV_PASS", priv_pass)
        .with_startup_timeout(STARTUP_TIMEOUT)
        .start()
        .await?;
    Ok(c)
}

/// Spin up mock-redfish-service. HTTP on TCP 8443 mapped to host.
pub async fn start_mock_redfish_service() -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("public.ecr.aws/y1d2j6a8/mock-redfish-service", "latest")
        .with_exposed_port(ContainerPort::Tcp(8443))
        .with_wait_for(WaitFor::message_on_stdout("mock-redfish-service listening"))
        .with_startup_timeout(STARTUP_TIMEOUT)
        .start()
        .await?;
    Ok(c)
}

/// Spin up mock-dnp3-outstation. TCP 20000 mapped to host.
pub async fn start_mock_dnp3_outstation() -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("public.ecr.aws/y1d2j6a8/mock-dnp3-outstation", "latest")
        .with_exposed_port(ContainerPort::Tcp(20000))
        .with_wait_for(WaitFor::message_on_stdout("mock-dnp3-outstation listening"))
        .with_startup_timeout(STARTUP_TIMEOUT)
        .start()
        .await?;
    Ok(c)
}

/// Spin up mock-bacnet-device. UDP 47808 mapped; gateway reaches it via
/// host port. Not on the shared Docker network.
pub async fn start_mock_bacnet_device() -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("public.ecr.aws/y1d2j6a8/mock-bacnet-device", "latest")
        .with_exposed_port(ContainerPort::Udp(47808))
        .with_wait_for(WaitFor::message_on_stdout("mock-bacnet-device listening"))
        .with_startup_timeout(STARTUP_TIMEOUT)
        .start()
        .await?;
    Ok(c)
}

/// Spin up the real device-api with `ENV=beta` so it resolves `postgres` +
/// `hivemq` via `network` — must be the same network `start_postgres`/
/// `start_hivemq` were given for this test.
pub async fn start_device_api(network: &str) -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("public.ecr.aws/y1d2j6a8/ems-device-api", "latest")
        .with_exposed_port(ContainerPort::Tcp(3000))
        .with_wait_for(WaitFor::message_on_stdout(
            "Nest application successfully started",
        ))
        .with_env_var("ENV", "beta")
        .with_env_var(
            "DOCUMENT_URL",
            "postgres://postgres:test@postgres:5432/postgres",
        )
        // device-api gained an auth module (backend spec-alarms work) that
        // hard-requires these 4 at boot — operator/viewer login passwords +
        // their broker creds. Test values; real ones are deploy secrets.
        .with_env_var("AUTH_OPERATOR_PW", "test-operator-pw")
        .with_env_var("AUTH_VIEWER_PW", "test-viewer-pw")
        .with_env_var("MQTT_OPERATOR_PASSWORD", "test-operator-pw")
        .with_env_var("MQTT_VIEWER_PASSWORD", "test-viewer-pw")
        .with_network(network)
        .with_startup_timeout(STARTUP_TIMEOUT)
        .start()
        .await?;
    Ok(c)
}
