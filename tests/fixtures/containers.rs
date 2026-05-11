//! Testcontainer helpers for the gateway e2e test.

use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

/// Spin up Postgres for device-api.
pub async fn start_postgres() -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("postgres", "15")
        .with_exposed_port(ContainerPort::Tcp(5432))
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_PASSWORD", "test")
        .start()
        .await?;
    Ok(c)
}

/// Spin up emqx broker.
pub async fn start_emqx() -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("emqx/emqx", "latest")
        .with_exposed_port(ContainerPort::Tcp(1883))
        .with_wait_for(WaitFor::message_on_stdout(
            "Listener tcp:default on 0.0.0.0:1883 started.",
        ))
        .start()
        .await?;
    Ok(c)
}

/// Spin up mock-modbus-server fixture.
pub async fn start_mock_modbus_server() -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("173.211.12.43:8083/library/mock-modbus-server", "latest")
        .with_exposed_port(ContainerPort::Tcp(502))
        .with_wait_for(WaitFor::message_on_stdout("mock-modbus-server listening"))
        .start()
        .await?;
    Ok(c)
}

/// Spin up the real device-api. Caller wires Postgres + emqx hostnames via env.
pub async fn start_device_api(
    postgres_host: &str,
    postgres_port: u16,
    emqx_host: &str,
    emqx_port: u16,
) -> anyhow::Result<ContainerAsync<GenericImage>> {
    let c = GenericImage::new("173.211.12.43:8083/library/ems-device-api", "latest")
        .with_exposed_port(ContainerPort::Tcp(3000))
        .with_wait_for(WaitFor::message_on_stdout(
            "Nest application successfully started",
        ))
        .with_env_var("POSTGRES_HOST", postgres_host)
        .with_env_var("POSTGRES_PORT", postgres_port.to_string())
        .with_env_var("POSTGRES_PASSWORD", "test")
        .with_env_var("MQTT_BROKER_URL", format!("mqtt://{emqx_host}:{emqx_port}"))
        .start()
        .await?;
    Ok(c)
}
