//! The `sunspec` binding as device-api emits it: model + point, plus the
//! Modbus TCP connection SunSpec rides on.

use super::{AsyncApiSpec, CommandSource, ProtocolBinding, ProtocolSource};
use serde_json::json;

#[test]
fn a_sunspec_measurement_names_its_model_and_point() {
    // Arrange — model 103 St, an enum labelled through value_map
    let entry = json!({
        "unit": "none", "poll_rate_hz": 1.0,
        "protocol": "sunspec", "host": "10.0.0.7", "port": 502, "unit_id": "1",
        "model_id": 103, "point": "St", "bit": null,
        "value_map": { "4": "MPPT" },
    });
    // Act
    let src: ProtocolSource = serde_json::from_value(entry).unwrap();
    // Assert
    let ProtocolBinding::Sunspec(b) = src.binding else {
        panic!("expected Sunspec variant");
    };
    assert_eq!(
        (b.host.as_str(), b.port, b.unit_id.as_str()),
        ("10.0.0.7", 502, "1")
    );
    assert_eq!((b.model_id, b.point.as_str(), b.bit), (103, "St", None));
    assert_eq!(src.value_map.unwrap()["4"], "MPPT");
}

#[test]
fn a_sunspec_command_can_name_one_bit_of_a_bitfield() {
    // Arrange — EPC 65534 CmdBits bit 0, the run enable
    let entry = json!({
        "verb": "enable", "target": "run", "unit": "none",
        "protocol": "sunspec", "host": "10.0.0.7", "port": 502, "unit_id": "1",
        "model_id": 65534, "point": "CmdBits", "bit": 0,
    });
    // Act
    let src: CommandSource = serde_json::from_value(entry).unwrap();
    // Assert
    let ProtocolBinding::Sunspec(b) = src.binding else {
        panic!("expected Sunspec variant");
    };
    assert_eq!(
        (b.model_id, b.point.as_str(), b.bit),
        (65534, "CmdBits", Some(0))
    );
}

#[test]
fn a_device_binding_a_point_the_gateway_doesnt_ship_is_skipped() {
    // Arrange — pcs_1 reads a real point; pcs_2 a model the gateway lacks;
    // pcs_3 commands a read-only point
    let point = |model_id: u16, point: &str| {
        json!({
            "unit": "watts", "poll_rate_hz": 1.0, "protocol": "sunspec",
            "host": "10.0.0.7", "port": 502, "unit_id": "1",
            "model_id": model_id, "point": point,
        })
    };
    let mut read_only = point(103, "W");
    read_only["verb"] = json!("set");
    read_only["target"] = json!("active_power");
    read_only.as_object_mut().unwrap().remove("poll_rate_hz");
    let spec = json!({
        "info": { "version": "v1" },
        "x-protocol-source": {
            "pcs_1": { "active_power": point(103, "W") },
            "pcs_2": { "active_power": point(701, "W") },
        },
        "x-command-source": { "pcs_3": { "set_active_power": read_only } },
    });
    // Act
    let parsed: AsyncApiSpec = serde_json::from_value(spec).unwrap();
    // Assert
    assert!(parsed.x_protocol_source.contains_key("pcs_1"));
    assert!(!parsed.x_protocol_source.contains_key("pcs_2"));
    assert!(!parsed.x_command_source.contains_key("pcs_3"));
}
