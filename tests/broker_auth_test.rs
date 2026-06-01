//! e2e: authenticated MQTT against platform's `ems-hivemq` File-RBAC broker.
//!
//! Three asserts (per platform's "Done =" block in handoff-ics-gateway-broker-auth):
//!   1. Gateway connects with `arcnode_gateway` + correct password.
//!   2. Wrong password → not_authorized fail-loud (no retry-loop).
//!   3. Subscribe to `commands/#` succeeds; publish to `commands/*` is rejected.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::mqtt::publisher;
use fixtures::containers::start_ems_hivemq_with_credentials;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use std::path::PathBuf;
use std::time::Duration;

fn credentials_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/credentials.xml")
}

#[tokio::test]
async fn connects_with_arcnode_gateway_and_publishes_to_measurements() -> Result<()> {
    // Arrange — spin ems-hivemq with the test credentials.xml mounted.
    let broker = start_ems_hivemq_with_credentials(&credentials_path()).await?;
    let port = broker.get_host_port_ipv4(1883).await?;
    let url = format!("tcp://localhost:{port}");

    // Act — connect with the platform's static username + the test password.
    let client = publisher::connect(&url, "broker-auth-test-1", "arcnode_gateway", "test").await?;

    // Assert — publish to measurements ACL'd topic resolves cleanly.
    publisher::publish_measurement(
        &client,
        "sites/site_001/devices/dev_a/measurements/active_power/watts",
        12_345.0,
    )
    .await?;
    client.disconnect(None).await?;
    Ok(())
}

#[tokio::test]
async fn wrong_password_fails_loud_with_not_authorized() -> Result<()> {
    // Arrange
    let broker = start_ems_hivemq_with_credentials(&credentials_path()).await?;
    let port = broker.get_host_port_ipv4(1883).await?;
    let url = format!("tcp://localhost:{port}");

    // Act — wrong password
    let res = publisher::connect(&url, "broker-auth-test-2", "arcnode_gateway", "WRONG").await;

    // Assert — Err with `not_authorized` marker so app::run can match + exit.
    let err = match res {
        Err(e) => e,
        Ok(_) => panic!("wrong password must error"),
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("not_authorized"),
        "expected `not_authorized` in error, got: {msg}"
    );
    Ok(())
}

#[tokio::test]
async fn publish_to_commands_topic_is_rejected_by_acl() -> Result<()> {
    // Arrange — connect as the authed gateway client.
    let broker = start_ems_hivemq_with_credentials(&credentials_path()).await?;
    let port = broker.get_host_port_ipv4(1883).await?;
    let url = format!("tcp://localhost:{port}");
    let raw = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&url)
            .client_id("broker-auth-test-3")
            .finalize(),
    )?;
    raw.connect(
        ConnectOptionsBuilder::new()
            .user_name("arcnode_gateway")
            .password("test")
            .clean_session(true)
            .finalize(),
    )
    .await?;

    // Subscribe to commands/# — gateway role HAS this permission, must succeed.
    raw.subscribe("sites/+/devices/+/commands/#", 1).await?;

    // Publish to commands/* — gateway role does NOT have PUBLISH on this branch.
    // Plugin disconnects the client; subsequent ops must observe the disconnect.
    let msg = Message::new(
        "sites/site_001/devices/dev_a/commands/set/active_power/watts",
        r#"{"value":1.0}"#,
        1,
    );
    let _ = raw.publish(msg).await;

    // Give the broker a moment to drop the connection on the ACL violation.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Assert — broker has disconnected the client.
    assert!(
        !raw.is_connected(),
        "broker should drop client after unauthorized PUBLISH to commands/*"
    );
    Ok(())
}
