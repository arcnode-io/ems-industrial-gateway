//! Unit tests for the dispatch lifecycle contract (event payload shape,
//! command-frame parsing).

use super::{CommandFrame, Phase, event_payload};

#[test]
fn event_payload_carries_contract_fields() {
    // Arrange + Act
    let done = event_payload("2026-07-03T00:00:00Z", "cmd-1", Phase::Done, None);
    let failed = event_payload(
        "2026-07-03T00:00:00Z",
        "cmd-2",
        Phase::Failed,
        Some("unknown device x"),
    );
    // Assert — exact wire contract per dispatchEvents.ts
    let d: serde_json::Value = serde_json::from_str(&done).unwrap();
    assert_eq!(d["phase"], "done");
    assert_eq!(d["command_id"], "cmd-1");
    assert!(d.get("reason").is_none());
    let f: serde_json::Value = serde_json::from_str(&failed).unwrap();
    assert_eq!(f["phase"], "failed");
    assert_eq!(f["reason"], "unknown device x");
}

#[test]
fn command_frame_requires_command_id() {
    // Arrange — frame missing command_id (HMI can't correlate an ack)
    let bad = br#"{"ts":"t","value":1.0}"#;
    // Act + Assert
    assert!(serde_json::from_slice::<CommandFrame>(bad).is_err());
}

#[tokio::test]
async fn a_redfish_setpoint_is_written_to_the_bmc() {
    use super::execute_setpoint;
    use crate::asyncapi::types::ProtocolBinding;
    use crate::synthetic::new_input_cache;
    use serde_json::json;
    use std::collections::HashMap;
    use wiremock::matchers::{body_json, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    // Arrange — a GPU's power limit, as gpu_node's command binds it
    let bmc = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(body_json(
            json!({ "PowerLimitWatts": { "SetPoint": 810.0 } }),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&bmc)
        .await;
    let binding: ProtocolBinding = serde_json::from_value(json!({
        "protocol": "redfish", "host": "127.0.0.1", "port": bmc.address().port(),
        "uri": "/Systems/HGX_Baseboard_0/Processors/GPU_SXM_1/EnvironmentMetrics",
        "json_pointer": "/PowerLimitWatts/SetPoint",
    }))
    .unwrap();
    // Act
    let written = execute_setpoint(
        &binding,
        810.0,
        "gpu_node_01",
        "set_gpu_1_power_limit",
        "s",
        &HashMap::new(),
        &HashMap::new(),
        None,
        &new_input_cache(),
    )
    .await;
    // Assert
    assert!(written.is_ok(), "{written:?}");
}
