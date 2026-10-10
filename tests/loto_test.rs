//! e2e: a locked-out device's command fails with the lockout as its reason,
//! and `system/loto_changed` lifts it. The rack's Modbus binding points at a
//! closed port, so once unlocked its command fails on the write instead:
//! the two reasons tell the lock from the device apart. A gateway whose
//! `/loto` won't answer doesn't start.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{start_hivemq, unique_network};
use fixtures::spec_stub::spawn_lockable_stub;
use futures::StreamExt;
use paho_mqtt::{AsyncClient, AsyncReceiver, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SITE_ID: &str = "local_site";
const RACK: &str = "rack_1";
const LOCKED_REASON: &str = "device rack_1 is locked out";

/// A rack with a Modbus `set_active_power` on a port nothing listens on.
fn spec() -> Value {
    json!({
        "info": { "version": "v1" },
        "x-protocol-source": {},
        "x-command-source": {
            RACK: { "set_active_power": {
                "verb": "set", "target": "active_power", "unit": "watts",
                "protocol": "modbus_tcp", "host": "127.0.0.1", "port": 1,
                "unit_id": "1", "address": 40, "scale": 1.0, "offset": 0.0,
            }},
        },
    })
}

fn config(device_api_url: String, broker_url: String) -> Config {
    unsafe {
        std::env::set_var("MQTT_GATEWAY_PASSWORD", "test");
    }
    Config {
        device_api_url,
        broker_url,
        mqtt_username: "arcnode_gateway".to_string(),
        site_id: SITE_ID.to_string(),
        log_level: "info".to_string(),
        gateway_credentials: None,
    }
}

#[tokio::test]
async fn a_locked_rack_refuses_commands_until_the_lock_clears() -> Result<()> {
    // Arrange — rack_1 locked out from boot
    let hivemq = start_hivemq(&unique_network()).await?;
    let broker_url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let (stub, locked) = spawn_lockable_stub(spec()).await;
    *locked.lock().unwrap() = vec![RACK.to_string()];
    let (op, mut acks) = operator(&broker_url).await?;
    let cancel = CancellationToken::new();
    let gateway = tokio::spawn(app::run(config(stub.uri(), broker_url), cancel.clone()));

    // Act + Assert — refused for the lock
    assert_eq!(command_reason(&op, &mut acks, "c1").await?, LOCKED_REASON);

    // Act — the lock clears and device-api beacons
    locked.lock().unwrap().clear();
    op.publish(Message::new("system/loto_changed", r#"{"ts":"t"}"#, 1))
        .await?;

    // Assert — the command now reaches the write (and fails on the closed port)
    let unlocked = timeout(Duration::from_secs(90), async {
        for n in 0.. {
            let reason = command_reason(&op, &mut acks, &format!("u{n}")).await?;
            if reason != LOCKED_REASON {
                return anyhow::Ok(reason);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        unreachable!()
    })
    .await??;
    assert!(!unlocked.contains("locked out"), "{unlocked}");

    cancel.cancel();
    gateway.await??;
    Ok(())
}

#[tokio::test]
async fn a_gateway_whose_loto_wont_answer_doesnt_start() -> Result<()> {
    // Arrange — /asyncapi answers, /loto is missing
    let hivemq = start_hivemq(&unique_network()).await?;
    let broker_url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let stub = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/asyncapi"))
        .respond_with(ResponseTemplate::new(200).set_body_json(spec()))
        .mount(&stub)
        .await;
    // Act
    let run = app::run(config(stub.uri(), broker_url), CancellationToken::new());
    let result = timeout(Duration::from_secs(30), run).await?;
    // Assert
    let err = format!("{:#}", result.expect_err("gateway started without /loto"));
    assert!(err.contains("/loto"), "{err}");
    Ok(())
}

/// An operator client subscribed to rack_1's dispatch acks.
async fn operator(broker_url: &str) -> Result<(AsyncClient, AsyncReceiver<Option<Message>>)> {
    let mut op = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(broker_url)
            .client_id("loto-test-operator")
            .finalize(),
    )?;
    let acks = op.get_stream(64);
    op.connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    op.subscribe(
        format!("sites/{SITE_ID}/devices/{RACK}/events/dispatch_state"),
        1,
    )
    .await?;
    Ok((op, acks))
}

/// Send rack_1 a 100 kW command (re-sent until the gateway is listening) and
/// return its `failed` reason.
async fn command_reason(
    op: &AsyncClient,
    acks: &mut AsyncReceiver<Option<Message>>,
    command_id: &str,
) -> Result<String> {
    let topic = format!("sites/{SITE_ID}/devices/{RACK}/commands/set/active_power/watts");
    let payload = json!({ "ts": "t", "value": 100_000.0, "command_id": command_id }).to_string();
    timeout(Duration::from_secs(60), async {
        loop {
            op.publish(Message::new(topic.as_str(), payload.as_str(), 1))
                .await?;
            let waited = timeout(Duration::from_secs(2), async {
                while let Some(msg) = acks.next().await {
                    let Some(msg) = msg else { continue };
                    let ack: Value = serde_json::from_slice(msg.payload())?;
                    if ack["command_id"] == command_id && ack["phase"] == "failed" {
                        return anyhow::Ok(ack["reason"].as_str().unwrap_or_default().to_string());
                    }
                }
                anyhow::bail!("ack stream closed")
            })
            .await;
            if let Ok(reason) = waited {
                return reason;
            }
        }
    })
    .await?
}
