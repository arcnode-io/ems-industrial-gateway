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

/// Payload for a float measurement reading.
#[derive(Debug, Serialize)]
pub struct FloatSample {
    /// ISO-8601 timestamp of the reading.
    pub ts: String,
    /// Engineering-unit value after scale + offset.
    pub value: f64,
}

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
        .finalize();
    let client = AsyncClient::new(create_opts).context("create mqtt client")?;
    let conn_opts = ConnectOptionsBuilder::new()
        .keep_alive_interval(Duration::from_secs(20))
        .clean_session(true)
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

/// Publish a FloatSample at QoS 1 to the given topic.
pub async fn publish_measurement(client: &AsyncClient, topic: &str, value: f64) -> Result<()> {
    let sample = FloatSample {
        ts: Utc::now().to_rfc3339(),
        value,
    };
    let payload = serde_json::to_vec(&sample).context("serialize FloatSample")?;
    let msg = Message::new(topic, payload, 1);
    client.publish(msg).await.context("mqtt publish")?;
    Ok(())
}
