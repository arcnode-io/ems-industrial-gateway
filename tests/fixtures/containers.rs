//! Testcontainer helpers for the gateway e2e test.
//!
//! Postgres / hivemq / device-api join a shared Docker network so device-api
//! resolves `postgres` and `hivemq` hostnames per its `beta:` cfg block.
//! mock-modbus-server doesn't need the network — the gateway (running on the
//! host) reaches it via the testcontainer's mapped port.

use std::time::Duration;
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

/// Generous container-startup ceiling. Default in testcontainers-rs is 60s,
/// which is tight when a CI runner is cold-pulling 7 images in parallel for
/// the big 5-protocol integration test. 180s gives slack without masking
/// genuine startup bugs (those usually fail in <10s).
const STARTUP_TIMEOUT: Duration = Duration::from_secs(180);

/// Shared Docker network name for the e2e stack.
pub const NETWORK: &str = "gateway-e2e";

/// Spin up Postgres on the shared network with hostname `postgres`.
pub async fn start_postgres() -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("postgres", "15")
        .with_exposed_port(ContainerPort::Tcp(5432))
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_PASSWORD", "test")
        .with_network(NETWORK)
        .with_container_name("postgres")
        .with_startup_timeout(STARTUP_TIMEOUT)
        .start()
        .await?;
    Ok(c)
}

/// Spin up hivemq on the shared network with hostname `hivemq`.
///
/// Uses `with_mapped_port(0, ...)` (single OS-assigned host binding)
/// rather than `with_exposed_port` because the latter triggers
/// testcontainers-rs's `publish_all_ports = true` branch — docker `-P`
/// mode publishes EVERY image-EXPOSE port (HiveMQ exposes 1883/8000/
/// 8083/8443/8883), multiplying collision odds on a busy CI runner
/// (e.g. Harbor on 8083).
pub async fn start_hivemq() -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("hivemq/hivemq-ce", "latest")
        .with_wait_for(WaitFor::message_on_stdout(
            "Started TCP Listener on address 0.0.0.0 and on port 1883.",
        ))
        .with_mapped_port(0, ContainerPort::Tcp(1883))
        .with_network(NETWORK)
        .with_container_name("hivemq")
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
/// from the host via mapped port.
pub async fn start_mock_modbus_server() -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("public.ecr.aws/y1d2j6a8/mock-modbus-server", "latest")
        .with_exposed_port(ContainerPort::Tcp(502))
        .with_wait_for(WaitFor::message_on_stdout("mock-modbus-server listening"))
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
/// `hivemq` via the shared Docker network.
pub async fn start_device_api() -> anyhow::Result<ContainerAsync<GenericImage>> {
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
        .with_network(NETWORK)
        .with_startup_timeout(STARTUP_TIMEOUT)
        .start()
        .await?;
    Ok(c)
}
