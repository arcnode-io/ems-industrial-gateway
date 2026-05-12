//! Testcontainer helpers for the gateway e2e test.
//!
//! Postgres / emqx / device-api join a shared Docker network so device-api
//! resolves `postgres` and `emqx` hostnames per its `beta:` cfg block.
//! mock-modbus-server doesn't need the network — the gateway (running on the
//! host) reaches it via the testcontainer's mapped port.

use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

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
        .start()
        .await?;
    Ok(c)
}

/// Spin up emqx on the shared network with hostname `emqx`.
pub async fn start_emqx() -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("emqx/emqx", "latest")
        .with_exposed_port(ContainerPort::Tcp(1883))
        .with_wait_for(WaitFor::message_on_stdout(
            "Listener tcp:default on 0.0.0.0:1883 started.",
        ))
        .with_network(NETWORK)
        .with_container_name("emqx")
        .start()
        .await?;
    Ok(c)
}

/// Spin up mock-modbus-server. Not on the shared network — gateway reaches it
/// from the host via mapped port.
pub async fn start_mock_modbus_server() -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("173.211.12.43:8083/library/mock-modbus-server", "latest")
        .with_exposed_port(ContainerPort::Tcp(502))
        .with_wait_for(WaitFor::message_on_stdout("mock-modbus-server listening"))
        .start()
        .await?;
    Ok(c)
}

/// Spin up mock-snmp-agent. UDP 161 mapped; gateway reaches it via host port.
pub async fn start_mock_snmp_agent() -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("173.211.12.43:8083/library/mock-snmp-agent", "latest")
        .with_exposed_port(ContainerPort::Udp(161))
        .with_wait_for(WaitFor::message_on_stdout("mock-snmp-agent listening"))
        .start()
        .await?;
    Ok(c)
}

/// Spin up mock-redfish-service. HTTP on TCP 8443 mapped to host.
pub async fn start_mock_redfish_service() -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("173.211.12.43:8083/library/mock-redfish-service", "latest")
        .with_exposed_port(ContainerPort::Tcp(8443))
        .with_wait_for(WaitFor::message_on_stdout("mock-redfish-service listening"))
        .start()
        .await?;
    Ok(c)
}

/// Spin up the real device-api with `ENV=beta` so it resolves `postgres` +
/// `emqx` via the shared Docker network.
pub async fn start_device_api() -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("173.211.12.43:8083/library/ems-device-api", "latest")
        .with_exposed_port(ContainerPort::Tcp(3000))
        .with_wait_for(WaitFor::message_on_stdout(
            "Nest application successfully started",
        ))
        .with_env_var("ENV", "beta")
        .with_env_var("POSTGRES_PASSWORD", "test")
        .with_network(NETWORK)
        .start()
        .await?;
    Ok(c)
}
