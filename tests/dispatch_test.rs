//! e2e: the dispatch round-trip against the real File-RBAC broker.
//!
//! Operator publishes a command frame → the gateway's subscriber demux routes
//! it to dispatch::handle_command → the operator receives the lifecycle acks
//! on events/dispatch_state. Asserts the locked HMI contract
//! (dispatchEvents.ts): received → done for a spec-known device,
//! received → failed(reason) for a ghost device.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::mqtt::{publisher, subscriber};
use ems_industrial_gateway::synthetic::new_input_cache;
use fixtures::containers::start_ems_hivemq_with_credentials;
use futures::stream::StreamExt;
use paho_mqtt::Message;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::timeout;

fn credentials_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/credentials.xml")
}

const SITE: &str = "site_001";
const COMMAND_TOPIC: &str = "sites/site_001/devices/dev_known/commands/set/active_power/watts";
const GHOST_COMMAND_TOPIC: &str =
    "sites/site_001/devices/dev_ghost/commands/set/active_power/watts";

/// Collect the next `n` dispatch events (phase, command_id, reason) from the
/// operator's stream, with a per-event timeout.
async fn collect_events(
    stream: &mut paho_mqtt::AsyncReceiver<Option<Message>>,
    n: usize,
) -> Result<Vec<(String, String, Option<String>)>> {
    let mut events = Vec::new();
    while events.len() < n {
        let msg = timeout(Duration::from_secs(10), stream.next())
            .await?
            .flatten()
            .ok_or_else(|| anyhow::anyhow!("stream closed before {n} events"))?;
        let v: Value = serde_json::from_slice(msg.payload())?;
        events.push((
            v["phase"].as_str().unwrap_or_default().to_string(),
            v["command_id"].as_str().unwrap_or_default().to_string(),
            v.get("reason").and_then(Value::as_str).map(str::to_string),
        ));
    }
    Ok(events)
}

#[tokio::test]
async fn operator_command_gets_received_then_done_then_failed_for_ghost() -> Result<()> {
    // Arrange — real File-RBAC broker; gateway subscriber with a known-device set.
    let broker = start_ems_hivemq_with_credentials(&credentials_path()).await?;
    let port = broker.get_host_port_ipv4(1883).await?;
    let url = format!("tcp://localhost:{port}");

    let mut gateway =
        publisher::connect(&url, "dispatch-test-gw", "arcnode_gateway", "test").await?;
    let known: BTreeSet<String> = BTreeSet::from(["dev_known".to_string()]);
    let _beacon_rx = subscriber::subscribe(
        &mut gateway,
        &[],
        new_input_cache(),
        SITE,
        Arc::new(RwLock::new(known)),
    )
    .await?;

    // Operator client — subscribes the events family like the HMI does.
    let mut operator =
        publisher::connect(&url, "dispatch-test-op", "arcnode_operator", "test").await?;
    let mut events_stream = operator.get_stream(64);
    operator
        .subscribe("sites/site_001/devices/+/events/dispatch_state", 1)
        .await?;

    // Act — dispatch to a device the spec knows.
    operator
        .publish(Message::new(
            COMMAND_TOPIC,
            r#"{"ts":"2026-07-03T00:00:00Z","value":1620000,"command_id":"cmd-1"}"#,
            1,
        ))
        .await?;
    let acks = collect_events(&mut events_stream, 2).await?;

    // Assert — received → done, correlated to cmd-1 (contract: done = accepted).
    assert_eq!(acks[0], ("received".into(), "cmd-1".into(), None));
    assert_eq!(acks[1], ("done".into(), "cmd-1".into(), None));

    // Act — dispatch to a ghost device.
    operator
        .publish(Message::new(
            GHOST_COMMAND_TOPIC,
            r#"{"ts":"2026-07-03T00:00:00Z","value":5,"command_id":"cmd-2"}"#,
            1,
        ))
        .await?;
    let acks = collect_events(&mut events_stream, 2).await?;

    // Assert — received → failed with a reason naming the device.
    assert_eq!(acks[0].0, "received");
    assert_eq!(acks[1].0, "failed");
    assert_eq!(acks[1].1, "cmd-2");
    assert!(
        acks[1]
            .2
            .as_deref()
            .unwrap_or_default()
            .contains("dev_ghost"),
        "reason should name the unknown device: {:?}",
        acks[1].2
    );

    operator.disconnect(None).await?;
    gateway.disconnect(None).await?;
    Ok(())
}

#[tokio::test]
async fn malformed_command_frame_is_dropped_without_acks() -> Result<()> {
    // Arrange
    let broker = start_ems_hivemq_with_credentials(&credentials_path()).await?;
    let port = broker.get_host_port_ipv4(1883).await?;
    let url = format!("tcp://localhost:{port}");

    let mut gateway =
        publisher::connect(&url, "dispatch-test-gw2", "arcnode_gateway", "test").await?;
    let _beacon_rx = subscriber::subscribe(
        &mut gateway,
        &[],
        new_input_cache(),
        SITE,
        Arc::new(RwLock::new(BTreeSet::from(["dev_known".to_string()]))),
    )
    .await?;
    let mut operator =
        publisher::connect(&url, "dispatch-test-op2", "arcnode_operator", "test").await?;
    let mut events_stream = operator.get_stream(64);
    operator
        .subscribe("sites/site_001/devices/+/events/dispatch_state", 1)
        .await?;

    // Act — no command_id: nothing to correlate an ack to.
    operator
        .publish(Message::new(COMMAND_TOPIC, r#"{"ts":"t","value":1}"#, 1))
        .await?;

    // Assert — no event arrives (bounded wait).
    let got = timeout(Duration::from_secs(3), events_stream.next()).await;
    assert!(got.is_err(), "malformed frame must not be acked: {got:?}");

    operator.disconnect(None).await?;
    gateway.disconnect(None).await?;
    Ok(())
}

#[tokio::test]
async fn late_subscriber_recovers_terminal_state_from_retained_event() -> Result<()> {
    // Arrange — gateway handles a command BEFORE anyone subscribes (the HMI
    // subscribes on confirm, ms after publishing — its SUBACK loses the race).
    let broker = start_ems_hivemq_with_credentials(&credentials_path()).await?;
    let port = broker.get_host_port_ipv4(1883).await?;
    let url = format!("tcp://localhost:{port}");
    let mut gateway =
        publisher::connect(&url, "dispatch-test-gw3", "arcnode_gateway", "test").await?;
    let _beacon_rx = subscriber::subscribe(
        &mut gateway,
        &[],
        new_input_cache(),
        SITE,
        Arc::new(RwLock::new(BTreeSet::from(["dev_known".to_string()]))),
    )
    .await?;
    let mut operator =
        publisher::connect(&url, "dispatch-test-op3", "arcnode_operator", "test").await?;

    // Act — command published with NO events subscription in place.
    operator
        .publish(Message::new(
            COMMAND_TOPIC,
            r#"{"ts":"2026-07-03T00:00:00Z","value":9,"command_id":"cmd-late"}"#,
            1,
        ))
        .await?;
    tokio::time::sleep(Duration::from_millis(800)).await; // let both acks land
    let mut events_stream = operator.get_stream(16);
    operator
        .subscribe("sites/site_001/devices/dev_known/events/dispatch_state", 1)
        .await?;
    let acks = collect_events(&mut events_stream, 1).await?;

    // Assert — the retained terminal state arrives despite subscribing late.
    assert_eq!(acks[0].0, "done");
    assert_eq!(acks[0].1, "cmd-late");

    operator.disconnect(None).await?;
    gateway.disconnect(None).await?;
    Ok(())
}
