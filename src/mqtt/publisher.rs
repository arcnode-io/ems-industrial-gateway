//! MQTT publisher: emits FloatSample to sites/.../measurements/<name>/<unit>.

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde::Serialize;
use std::time::Duration;

/// MQTT v3 CONNACK return code for "not authorized". Platform's File-RBAC
/// broker rejects bad creds with this; we surface it as a distinct error
/// so app::run can fail-loud instead of retry-looping a credential mistake.
const CONNACK_NOT_AUTHORIZED: i32 = 5;
/// First reconnect retry after a lost connection; paho doubles it per failure.
const RECONNECT_MIN: Duration = Duration::from_secs(1);
/// Cap on the reconnect retry interval.
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Payload for a float measurement reading.
#[derive(Debug, Serialize)]
pub struct FloatSample {
    /// ISO-8601 timestamp of the reading.
    pub ts: String,
    /// Engineering-unit value after scale + offset.
    pub value: f64,
}

/// QoS for measurements (ADR §18): periodic telemetry, the next reading
/// supersedes a lost one. Reason: nothing to track in flight, so a dropped
/// connection has no QoS 1 publications for paho to clean up (paho C 1.3.14
/// double-frees those, eclipse-paho/paho.mqtt.c#1622).
const QOS_MEASUREMENT: i32 = 0;

/// Outbound publishes paho may hold before it refuses more.
///
/// Reason: paho's default (100) applies while connected too, counting every
/// publish until its send thread writes it out. Each device poller and
/// synthetic task waits for its publish before the next, so outstanding
/// publishes never exceed the task count: hundreds per site, past 100 at
/// once on a busy tick. Sized well above any site's task count; a queued
/// sample is a few hundred bytes.
const MAX_BUFFERED_PUBLISHES: i32 = 10_000;

/// Build an MQTT client and connect with username/password.
///
/// `password` is the env-var secret `MQTT_GATEWAY_PASSWORD`; caller pulls it
/// from env so this fn stays unit-testable. On CONNACK NotAuthorized the
/// returned error message contains `not_authorized` so app::run can match
/// + exit hard rather than retry-loop.
pub async fn connect(
    broker_url: &str,
    client_id: &str,
    username: &str,
    password: &str,
) -> Result<AsyncClient> {
    let create_opts = CreateOptionsBuilder::new()
        .server_uri(broker_url)
        .client_id(client_id)
        .max_buffered_messages(MAX_BUFFERED_PUBLISHES)
        .finalize();
    let client = AsyncClient::new(create_opts).context("create mqtt client")?;
    let conn_opts = ConnectOptionsBuilder::new()
        .keep_alive_interval(Duration::from_secs(20))
        .clean_session(true)
        // Reason: paho's reconnect is off by default; without it a broker
        // restart leaves the gateway running but permanently disconnected.
        // Subscriptions don't survive the reconnect — subscriber::subscribe
        // re-issues them from the connected callback.
        .automatic_reconnect(RECONNECT_MIN, RECONNECT_MAX)
        .user_name(username)
        .password(password)
        .finalize();
    match client.connect(conn_opts).await {
        Ok(_) => Ok(client),
        Err(e) => Err(classify_connect_error(&e)),
    }
}

/// Map a paho connect error to an anyhow error. A CONNACK NotAuthorized — or
/// a broker that drops the TCP connection on a denied auth (File-RBAC does
/// this rather than returning a typed CONNACK) — is annotated `not_authorized`
/// so app::run can fail-loud on a credential mistake instead of treating it
/// as transient. Any startup connect failure is fatal regardless; the
/// annotation is for the operator's log.
fn classify_connect_error(e: &paho_mqtt::Error) -> anyhow::Error {
    if let paho_mqtt::Error::ConnectReturn(rc) = e
        && (*rc as i32 == CONNACK_NOT_AUTHORIZED
            || matches!(rc, paho_mqtt::ConnectReturnCode::BadUserNameOrPassword))
    {
        return anyhow!("mqtt connect: not_authorized — bad credentials ({rc})");
    }
    let raw = format!("{e}");
    if raw.contains("Not authorized") || raw.contains("Bad User Name or Password") {
        anyhow!("mqtt connect: not_authorized — bad credentials: {raw}")
    } else {
        anyhow!("connect to broker: {raw}")
    }
}

/// Publish an already-shaped `{ts, value}` sample (see `payload`).
pub async fn publish_sample(
    client: &AsyncClient,
    topic: &str,
    sample: &serde_json::Value,
) -> Result<()> {
    let payload = serde_json::to_vec(sample).context("serialize sample")?;
    client
        .publish(Message::new(topic, payload, QOS_MEASUREMENT))
        .await
        .context("mqtt publish")?;
    Ok(())
}

/// Publish a FloatSample to the given topic.
pub async fn publish_measurement(client: &AsyncClient, topic: &str, value: f64) -> Result<()> {
    let sample = FloatSample {
        ts: Utc::now().to_rfc3339(),
        value,
    };
    let payload = serde_json::to_vec(&sample).context("serialize FloatSample")?;
    let msg = Message::new(topic, payload, QOS_MEASUREMENT);
    client.publish(msg).await.context("mqtt publish")?;
    Ok(())
}
