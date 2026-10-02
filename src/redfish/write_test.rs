//! Redfish writes: a setpoint lands as a PATCH of the binding's resource,
//! the value nested at its JSON Pointer (DSP0266 §7.6, partial update).

use super::write_setpoint;
use crate::asyncapi::types::RedfishBinding;
use serde_json::json;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const GPU_1: &str = "/Systems/HGX_Baseboard_0/Processors/GPU_SXM_1/EnvironmentMetrics";

fn gpu_power_limit(port: u16) -> RedfishBinding {
    serde_json::from_value(json!({
        "host": "127.0.0.1", "port": port, "uri": GPU_1,
        "json_pointer": "/PowerLimitWatts/SetPoint",
    }))
    .unwrap()
}

#[tokio::test]
async fn a_setpoint_is_patched_at_its_pointer() {
    // Arrange
    let bmc = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path(format!("/redfish/v1{GPU_1}")))
        .and(body_json(
            json!({ "PowerLimitWatts": { "SetPoint": 700.0 } }),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&bmc)
        .await;
    // Act
    let written = write_setpoint(&gpu_power_limit(bmc.address().port()), 700.0, None, None).await;
    // Assert
    assert!(written.is_ok(), "{written:?}");
}

#[tokio::test]
async fn a_refused_setpoint_fails_with_the_bmcs_status() {
    // Arrange — out of the GPU's allowable range
    let bmc = MockServer::start().await;
    Mock::given(method("PATCH"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&bmc)
        .await;
    // Act
    let written = write_setpoint(&gpu_power_limit(bmc.address().port()), 50.0, None, None).await;
    // Assert
    let err = format!("{:#}", written.unwrap_err());
    assert!(err.contains("400"), "{err}");
}
