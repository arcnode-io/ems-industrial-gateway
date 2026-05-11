//! HTTP client for fetching `/asyncapi` from device-api with exponential
//! backoff. Returns a fully validated `AsyncApiSpec`.

use crate::asyncapi::types::AsyncApiSpec;
use anyhow::{Context, Result};
use backoff::ExponentialBackoff;
use backoff::future::retry;
use reqwest::Client;
use std::time::Duration;
use tracing::warn;
use validator::Validate;

/// Fetch `/asyncapi`, deserialize into `AsyncApiSpec`, and validate.
/// Retries with exponential backoff on transient failures (handles boot-time
/// race when device-api is still warming up).
pub async fn fetch_asyncapi(base_url: &str) -> Result<AsyncApiSpec> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("build reqwest client")?;
    let url = format!("{}/asyncapi", base_url);

    let backoff = ExponentialBackoff {
        initial_interval: Duration::from_secs(1),
        max_elapsed_time: Some(Duration::from_secs(60)),
        ..Default::default()
    };

    let body = retry(backoff, || async {
        let resp = client.get(&url).send().await.map_err(|e| {
            warn!(error = %e, "fetch_asyncapi attempt failed; retrying");
            backoff::Error::transient(anyhow::anyhow!(e))
        })?;
        if resp.status().is_server_error() {
            warn!(status = %resp.status(), "fetch_asyncapi got 5xx; retrying");
            return Err(backoff::Error::transient(anyhow::anyhow!(
                "server error: {}",
                resp.status()
            )));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| backoff::Error::permanent(anyhow::anyhow!(e)))?;
        Ok(text)
    })
    .await
    .context("fetch /asyncapi exceeded backoff window")?;

    let spec: AsyncApiSpec =
        serde_json::from_str(&body).context("parse /asyncapi into AsyncApiSpec")?;
    spec.validate().context("/asyncapi failed validation")?;
    Ok(spec)
}
