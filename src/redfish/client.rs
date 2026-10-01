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
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, OnceLock};
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
        (Some(DeviceTrust::TlsMutual { .. }), Some(creds)) => (https_client(creds)?, "https"),
        _ => (plain_client()?, "http"),
    };
    let url = format!("{}://{}:{}/redfish/v1{}", scheme, b.host, b.port, b.uri);

    let body = fetch(&client, &url).await?;
    let value: &Value = match &b.json_pointer {
        Some(ptr) => body
            .pointer(ptr)
            .with_context(|| format!("json pointer {ptr} missed in response from {url}"))?,
        None => &body,
    };
    match (value, &b.value_map) {
        (Value::String(text), Some(map)) => map
            .get(text)
            .copied()
            .with_context(|| format!("Redfish value {text:?} at {url} is not in the value_map")),
        _ => {
            let raw = value.as_f64().with_context(|| {
                format!("expected numeric Redfish value at {url}, got {value:?}")
            })?;
            Ok(raw * b.scale)
        }
    }
}

/// Process-wide plain HTTP client.
///
/// Reason: a `Client` pools connections, so building one per read opened a
/// new TCP connection per poll. At ~100 BMCs polled every second, the closed
/// ones pile up in TIME-WAIT and exhaust the host's ephemeral ports.
fn plain_client() -> Result<Client> {
    static PLAIN: OnceLock<Client> = OnceLock::new();
    if let Some(client) = PLAIN.get() {
        return Ok(client.clone());
    }
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("build reqwest plain Client")?;
    Ok(PLAIN.get_or_init(|| client).clone())
}

/// HTTPS+mTLS client, cached per credential set (keyed by its three paths)
/// so a process can never reuse a client built with a different identity.
/// Also avoids re-reading the cert files and a full TLS handshake per poll.
fn https_client(creds: &GatewayCredentials) -> Result<Client> {
    type CredsKey = (PathBuf, PathBuf, PathBuf);
    static MTLS: LazyLock<Mutex<HashMap<CredsKey, Client>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let key = (
        creds.ca_bundle_path.clone(),
        creds.cert_path.clone(),
        creds.key_path.clone(),
    );
    if let Some(client) = MTLS.lock().unwrap().get(&key) {
        return Ok(client.clone());
    }
    let client = tls::build_https_client(creds)?;
    Ok(MTLS.lock().unwrap().entry(key).or_insert(client).clone())
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

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
