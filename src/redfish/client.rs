//! Redfish client. GETs a resource, optionally drills with JSON Pointer.
//! Plain HTTP + HTTPS+mTLS branches share the same fetch loop; only the
//! reqwest Client + URL scheme differ.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::RedfishBinding;
use crate::config::GatewayCredentials;
use crate::redfish::tls;
use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

/// Same retry curve as the other protocols — handles boot-time race.
const MAX_READ_ATTEMPTS: u32 = 5;

/// Full read pipeline for a Redfish measurement.
///
/// `trust = Some(TlsMutual{..})` + `creds = Some(..)` → HTTPS+mTLS dial
/// (DSP0266 §13.1 + §13.3.5). Else falls back to plain HTTP.
pub async fn read_measurement(
    b: &RedfishBinding,
    trust: Option<&DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<f64> {
    let (client, scheme) = match (trust, creds) {
        (Some(DeviceTrust::TlsMutual { .. }), Some(creds)) => {
            (tls::build_https_client(creds)?, "https")
        }
        _ => (build_plain_client()?, "http"),
    };
    let url = format!("{}://{}:{}/redfish/v1{}", scheme, b.host, b.port, b.uri);

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

/// Plain HTTP client. Existing behavior — kept for pre-trust DTMs.
fn build_plain_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("build reqwest plain Client")
}

/// HTTP(S) GET with exponential backoff on transient errors. Shared by both
/// branches — the `Client` carries TLS config (or not).
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
