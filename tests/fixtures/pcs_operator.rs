//! An operator on the broker for the SunSpec e2e: subscribes to pcs_1's
//! dispatch acks and active power, sends commands, reads replies.

use anyhow::Result;
use futures::StreamExt;
use paho_mqtt::{AsyncClient, AsyncReceiver, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::timeout;

const SITE_ID: &str = "local_site";
const PCS: &str = "pcs_1";

/// An operator subscribed to pcs_1's acks and active power.
pub async fn operator(broker_url: &str) -> Result<(AsyncClient, AsyncReceiver<Option<Message>>)> {
    let mut op = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(broker_url)
            .client_id(format!("sunspec-operator-{}", std::process::id()))
            .finalize(),
    )?;
    let inbox = op.get_stream(256);
    op.connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    let device = format!("sites/{SITE_ID}/devices/{PCS}");
    op.subscribe(format!("{device}/events/dispatch_state"), 1)
        .await?;
    op.subscribe(format!("{device}/measurements/active_power/watts"), 1)
        .await?;
    Ok((op, inbox))
}

/// The next active power reading.
pub async fn measurement(inbox: &mut AsyncReceiver<Option<Message>>) -> Result<f64> {
    timeout(Duration::from_secs(60), async {
        while let Some(msg) = inbox.next().await {
            let Some(msg) = msg else { continue };
            if msg.topic().ends_with("active_power/watts") {
                let v: Value = serde_json::from_slice(msg.payload())?;
                return Ok(v["value"].as_f64().unwrap_or(f64::NAN));
            }
        }
        anyhow::bail!("inbox closed")
    })
    .await?
}

/// Send one command (re-sent until the gateway is listening) and return
/// its final phase, with the reason on a failure.
pub async fn command(
    op: &AsyncClient,
    inbox: &mut AsyncReceiver<Option<Message>>,
    path: &str,
    command_id: &str,
    value: f64,
) -> Result<String> {
    let topic = format!("sites/{SITE_ID}/devices/{PCS}/commands/{path}");
    let payload = json!({ "ts": "t", "value": value, "command_id": command_id }).to_string();
    timeout(Duration::from_secs(60), async {
        loop {
            op.publish(Message::new(topic.as_str(), payload.as_str(), 1))
                .await?;
            let waited = timeout(Duration::from_secs(5), async {
                while let Some(msg) = inbox.next().await {
                    let Some(msg) = msg else { continue };
                    let Ok(ack) = serde_json::from_slice::<Value>(msg.payload()) else {
                        continue;
                    };
                    if ack["command_id"] != command_id {
                        continue;
                    }
                    match ack["phase"].as_str() {
                        Some("done") => return Ok("done".to_string()),
                        Some("failed") => return Ok(format!("failed: {}", ack["reason"])),
                        _ => {}
                    }
                }
                anyhow::bail!("inbox closed")
            })
            .await;
            if let Ok(phase) = waited {
                return phase;
            }
        }
    })
    .await?
}
