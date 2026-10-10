//! HTTP client for fetching `/asyncapi` and `/loto` from device-api with
//! exponential backoff.

use crate::asyncapi::types::AsyncApiSpec;
use crate::loto;
use anyhow::{Context, Result};
use backoff::ExponentialBackoff;
use backoff::future::retry;
use reqwest::Client;
use std::collections::HashSet;
use std::time::Duration;
use tracing::warn;
use validator::Validate;

/// Fetch `/asyncapi`, deserialize into `AsyncApiSpec`, and validate.
/// Retries with exponential backoff on transient failures (handles boot-time
/// race when device-api is still warming up).
pub async fn fetch_asyncapi(base_url: &str) -> Result<AsyncApiSpec> {
    let body = get_with_backoff(&format!("{base_url}/asyncapi")).await?;
    let spec: AsyncApiSpec =
        serde_json::from_str(&body).context("parse /asyncapi into AsyncApiSpec")?;
    spec.validate().context("/asyncapi failed validation")?;
    Ok(spec)
}

/// Fetch `/loto`: every locked-out device, subtrees expanded. Same retry as
/// `/asyncapi`.
pub async fn fetch_loto(base_url: &str) -> Result<HashSet<String>> {
    loto::parse(&get_with_backoff(&format!("{base_url}/loto")).await?)
}

/// GET `url`'s body, retrying transport errors and 5xx for up to 60 s. A
/// 4xx is final.
async fn get_with_backoff(url: &str) -> Result<String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("build reqwest client")?;
    let backoff = ExponentialBackoff {
        initial_interval: Duration::from_secs(1),
        max_elapsed_time: Some(Duration::from_secs(60)),
        ..Default::default()
    };
    retry(backoff, || async {
        let resp = client.get(url).send().await.map_err(|e| {
            warn!(%url, error = %e, "GET failed; retrying");
            backoff::Error::transient(anyhow::anyhow!(e))
        })?;
        if resp.status().is_server_error() {
            warn!(%url, status = %resp.status(), "GET got 5xx; retrying");
            return Err(backoff::Error::transient(anyhow::anyhow!(
                "server error: {}",
                resp.status()
            )));
        }
        let resp = resp
            .error_for_status()
            .map_err(|e| backoff::Error::permanent(anyhow::anyhow!(e)))?;
        resp.text()
            .await
            .map_err(|e| backoff::Error::permanent(anyhow::anyhow!(e)))
    })
    .await
    .with_context(|| format!("GET {url}"))
}
