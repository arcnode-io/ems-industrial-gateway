//! Redfish setpoint writes: PATCH the binding's resource with the value
//! nested at its JSON Pointer (DSP0266 §7.6, partial update). One attempt:
//! a refused write is the caller's to see, not to retry blindly.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::RedfishBinding;
use crate::config::GatewayCredentials;
use crate::redfish::client::{https_client, plain_client};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// Write `value` (engineering units) to the property `b` names.
///
/// `trust = Some(TlsMutual{..})` + `creds = Some(..)` → HTTPS+mTLS, same as
/// reads. Errors carry the BMC's status, e.g. a setpoint outside the
/// property's allowable range (400).
pub async fn write_setpoint(
    b: &RedfishBinding,
    value: f64,
    trust: Option<&DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<()> {
    let pointer = b
        .json_pointer
        .as_deref()
        .with_context(|| format!("redfish write to {} needs a json_pointer", b.uri))?;
    let (client, scheme) = match (trust, creds) {
        (Some(DeviceTrust::TlsMutual { .. }), Some(creds)) => (https_client(creds)?, "https"),
        _ => (plain_client()?, "http"),
    };
    let url = format!("{}://{}:{}/redfish/v1{}", scheme, b.host, b.port, b.uri);
    // Reason: DSP0266 §6.5 lets a service answer 428 to a PATCH without
    // If-Match, and NVIDIA's DGX BMCs expect one. `*` matches whatever the
    // resource currently is (RFC 7232), so it never trips a 412.
    let resp = client
        .patch(&url)
        .header("If-Match", "*")
        .json(&nest(pointer, json!(value / b.scale)))
        .send()
        .await
        .with_context(|| format!("PATCH {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("PATCH {url} refused: HTTP {status}");
    }
    Ok(())
}

/// `/A/B` + v → `{"A": {"B": v}}`, unescaping `~1` and `~0` (RFC 6901).
fn nest(pointer: &str, value: Value) -> Value {
    let tokens: Vec<&str> = pointer.split('/').skip(1).collect();
    tokens.into_iter().rev().fold(
        value,
        |inner, token| json!({ token.replace("~1", "/").replace("~0", "~"): inner }),
    )
}

#[cfg(test)]
#[path = "write_test.rs"]
mod tests;
