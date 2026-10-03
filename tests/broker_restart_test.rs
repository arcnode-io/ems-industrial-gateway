//! e2e: the gateway keeps receiving commands after the broker restarts.
//!
//! MQTT subscriptions are broker-side session state. A restarted broker
//! comes back with none, so a client that only reconnects the socket goes
//! permanently deaf with no error. Proven with a real HiveMQ restart: the
//! gateway acks every command for a device it controls with `received` (even
//! an unknown command), so an ack after the restart shows both reconnect and
//! resubscribe happened.

mod fixtures;

use anyhow::{Context, Result};
use ems_industrial_gateway::{app, config::Config};
use fixtures::spec_stub::spawn_asyncapi_stub;
use futures::stream::StreamExt;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde_json::{Value, json};
use std::time::Duration;
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const SITE_ID: &str = "local_site";
const BROKER_READY: &str = "Started TCP Listener on address 0.0.0.0 and on port 1883.";

/// HiveMQ on a fixed host port, so a restarted container comes back where
/// the gateway expects it (a `0` mapping gets a new random port on restart).
async fn start_hivemq_on(host_port: u16) -> Result<ContainerAsync<GenericImage>> {
    Ok(GenericImage::new("hivemq/hivemq-ce", "latest")
        .with_wait_for(WaitFor::message_on_stdout(BROKER_READY))
        .with_mapped_port(host_port, ContainerPort::Tcp(1883))
        .start()
        .await?)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Connect a fresh operator, then publish a command every second until the
/// gateway acks one with `received`. Fresh client each call: the operator's
/// own subscription must not be the thing under test.
async fn command_is_acked(broker_url: &str, attempt: &str) -> Result<()> {
    let mut op = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(broker_url)
            .client_id(format!("restart-op-{attempt}"))
            .finalize(),
    )?;
    let mut events = op.get_stream(64);
    // A restarted broker takes a few seconds to listen again.
    timeout(Duration::from_secs(60), async {
        while op
            .connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
            .await
            .is_err()
        {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await?;
    op.subscribe(
        format!("sites/{SITE_ID}/devices/+/events/dispatch_state"),
        1,
    )
    .await?;
    timeout(Duration::from_secs(60), async {
        let mut n = 0;
        loop {
            n += 1;
            op.publish(Message::new(
                format!("sites/{SITE_ID}/devices/probe/commands/set/active_power/watts"),
                format!(r#"{{"ts":"t","value":1,"command_id":"{attempt}-{n}"}}"#),
                1,
            ))
            .await?;
            if let Ok(Some(Some(msg))) = timeout(Duration::from_secs(1), events.next()).await {
                let v: Value = serde_json::from_slice(msg.payload())?;
                if v["phase"] == "received" {
                    return anyhow::Ok(());
                }
            }
        }
    })
    .await??;
    op.disconnect(None).await?;
    Ok(())
}

#[tokio::test]
async fn commands_still_arrive_after_a_broker_restart() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    // Arrange — broker + gateway with an empty spec (commands are acked anyway).
    let host_port = free_port();
    let broker = start_hivemq_on(host_port).await?;
    let broker_url = format!("tcp://localhost:{host_port}");
    let stub = spawn_asyncapi_stub(json!({
        "info": { "version": "v1" },
        "x-protocol-source": {},
        // A device the gateway controls, asked for a command it doesn't
        // have: acked received → failed with no south-side I/O.
        "x-command-source": { "probe": { "set_other_power": {
            "verb": "set", "target": "other_power", "unit": "watts",
            "protocol": "modbus_tcp", "host": "127.0.0.1", "port": 1, "unit_id": "1",
            "address": 0, "scale": 1.0, "offset": 0.0,
        } } },
    }))
    .await;
    unsafe {
        std::env::set_var("MQTT_GATEWAY_PASSWORD", "test");
    }
    let cfg = Config {
        device_api_url: stub.uri(),
        broker_url: broker_url.clone(),
        mqtt_username: "arcnode_gateway".to_string(),
        site_id: SITE_ID.to_string(),
        log_level: "info".to_string(),
        gateway_credentials: None,
    };
    let cancel = CancellationToken::new();
    let gateway = {
        let cancel = cancel.clone();
        tokio::spawn(async move { app::run(cfg, cancel).await })
    };
    command_is_acked(&broker_url, "before")
        .await
        .context("no ack BEFORE the restart")?;

    // Act — restart the broker; it comes back with no sessions at all.
    broker.stop_with_timeout(Some(1)).await?;
    broker.start().await?;

    // Assert — the gateway reconnected AND resubscribed on its own.
    command_is_acked(&broker_url, "after")
        .await
        .context("no ack AFTER the restart")?;

    cancel.cancel();
    gateway.await??;
    Ok(())
}
