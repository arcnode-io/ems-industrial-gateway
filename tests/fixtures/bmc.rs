//! Stub BMC reads: what the gateway's Redfish power-limit PATCHes sent.

use serde_json::Value;
use wiremock::MockServer;

/// Cap values (`PowerLimitWatts/SetPoint`) the BMC has been sent so far.
pub async fn caps_written(bmc: &MockServer) -> Vec<f64> {
    bmc.received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
        .filter_map(|b| {
            b.pointer("/PowerLimitWatts/SetPoint")
                .and_then(Value::as_f64)
        })
        .collect()
}
