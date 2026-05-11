//! MQTT publisher: emits FloatSample to sites/.../measurements/<name>/<unit>.

use anyhow::{Context, Result};
use chrono::Utc;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde::Serialize;
use std::time::Duration;

/// Payload for a float measurement reading.
#[derive(Debug, Serialize)]
pub struct FloatSample {
    /// ISO-8601 timestamp of the reading.
    pub ts: String,
    /// Engineering-unit value after scale + offset.
    pub value: f64,
}

/// Build an MQTT client and connect. Caller owns disconnect.
pub async fn connect(broker_url: &str, client_id: &str) -> Result<AsyncClient> {
    let create_opts = CreateOptionsBuilder::new()
        .server_uri(broker_url)
        .client_id(client_id)
        .finalize();
    let client = AsyncClient::new(create_opts).context("create mqtt client")?;
    let conn_opts = ConnectOptionsBuilder::new()
        .keep_alive_interval(Duration::from_secs(20))
        .clean_session(true)
        .finalize();
    client
        .connect(conn_opts)
        .await
        .context("connect to broker")?;
    Ok(client)
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
