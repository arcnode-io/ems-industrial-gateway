//! Fleet power cap: one percentage, applied to every child's own range.

use super::child_caps;
use crate::asyncapi::types::PowerCapBinding;
use serde_json::json;

fn two_gpus() -> PowerCapBinding {
    serde_json::from_value(json!({
        "children": [
            { "device_id": "gpu_node_01", "target": "gpu_1_power_limit", "min_w": 200.0, "max_w": 1000.0 },
            { "device_id": "gpu_node_02", "target": "gpu_1_power_limit", "min_w": 200.0, "max_w": 1000.0 },
        ],
    }))
    .unwrap()
}

#[test]
fn every_gpu_gets_the_same_percentage_of_its_max() {
    // Act
    let caps = child_caps(&two_gpus(), 81.0);
    // Assert
    assert_eq!(
        caps,
        vec![
            (
                "gpu_node_01".to_string(),
                "set_gpu_1_power_limit".to_string(),
                810.0
            ),
            (
                "gpu_node_02".to_string(),
                "set_gpu_1_power_limit".to_string(),
                810.0
            ),
        ]
    );
}

#[test]
fn a_cap_stays_inside_each_gpus_allowable_range() {
    // 10% of 1000 W is below the 200 W floor; 150% is above max
    assert_eq!(child_caps(&two_gpus(), 10.0)[0].2, 200.0);
    assert_eq!(child_caps(&two_gpus(), 150.0)[0].2, 1000.0);
}

#[tokio::test]
async fn a_fleet_cap_writes_every_gpus_own_limit() {
    use super::dispatch_power_cap;
    use crate::asyncapi::types::ProtocolBinding;
    use std::collections::HashMap;
    use wiremock::matchers::{body_json, method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    // Arrange — both gpu_nodes behind one BMC stub, each resolving its own
    // set_gpu_1_power_limit binding from x-command-source
    let bmc = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path_regex("GPU_SXM_1/EnvironmentMetrics$"))
        .and(body_json(
            json!({ "PowerLimitWatts": { "SetPoint": 810.0 } }),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&bmc)
        .await;
    let gpu_limit = || -> ProtocolBinding {
        serde_json::from_value(json!({
            "protocol": "redfish", "host": "127.0.0.1", "port": bmc.address().port(),
            "uri": "/Systems/HGX_Baseboard_0/Processors/GPU_SXM_1/EnvironmentMetrics",
            "json_pointer": "/PowerLimitWatts/SetPoint",
        }))
        .unwrap()
    };
    let channels: HashMap<String, HashMap<String, ProtocolBinding>> =
        ["gpu_node_01", "gpu_node_02"]
            .into_iter()
            .map(|d| {
                (
                    d.to_string(),
                    HashMap::from([("set_gpu_1_power_limit".to_string(), gpu_limit())]),
                )
            })
            .collect();
    // Act
    let written = dispatch_power_cap(&two_gpus(), 81.0, &channels, &HashMap::new(), None).await;
    // Assert — the stub's expect(2) verifies on drop
    assert!(written.is_ok(), "{written:?}");
}

#[test]
fn caps_are_whole_watts() {
    // The floor percentage comes out of float division (156.8 / 784 kW)
    let floor_percent = 156_800.0 / 784_000.0 * 100.0;
    assert_eq!(child_caps(&two_gpus(), floor_percent)[0].2, 200.0);
    // a fraction of a watt rounds up: a cap never cuts more than asked
    assert_eq!(child_caps(&two_gpus(), 81.04)[0].2, 811.0);
}
