//! Redfish HTTP client. GETs a resource, optionally drills with JSON Pointer.

use crate::asyncapi::types::RedfishBinding;
use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

/// Same retry curve as the other protocols — handles boot-time race.
const MAX_READ_ATTEMPTS: u32 = 5;

/// Full read pipeline for a Redfish measurement: GET resource, drill into
/// the JSON via the binding's pointer (if any), coerce to f64.
pub async fn read_measurement(b: &RedfishBinding) -> Result<f64> {
    let url = format!("http://{}:{}/redfish/v1{}", b.host, b.port, b.uri);
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("build reqwest client")?;

    let body = fetch(&client, &url).await?;
    let value: &Value = match &b.json_pointer {
        Some(ptr) => body
            .pointer(ptr)
            .with_context(|| format!("json pointer {ptr} missed in response from {url}"))?,
        None => &body,
    };
    value
        .as_f64()
        .with_context(|| format!("expected numeric Redfish value at {url}, got {value:?}"))
}

/// HTTP GET with exponential backoff on transient errors.
async fn fetch(client: &Client, url: &str) -> Result<Value> {
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..MAX_READ_ATTEMPTS {
        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                return resp.json().await.context("parse Redfish JSON");
            }
            Ok(resp) => {
                let status = resp.status();
                warn!(attempt, %status, "redfish non-success; retrying");
                last_err = Some(anyhow::anyhow!("redfish HTTP {status}"));
            }
            Err(e) => {
                warn!(attempt, error = %e, "redfish fetch failed; retrying");
                last_err = Some(anyhow::anyhow!(e));
            }
        }
        sleep(Duration::from_millis(500 * (1 << attempt))).await;
    }
    Err(last_err.unwrap()).context("redfish fetch exhausted retries")
}
