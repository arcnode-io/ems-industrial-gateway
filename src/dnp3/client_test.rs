//! DNP3 scale: relays report in their own primary units, so the raw point
//! value is scaled to the unit the measurement declares. Wrong here reads a
//! 13.8 kV bus as 13.8 V.

use super::client::scaled;
use crate::asyncapi::types::Dnp3TcpBinding;
use serde_json::json;

#[test]
fn scale_converts_kilovolts_primary_to_volts() {
    // Arrange — SEL-351 reports phase voltage in kV primary
    let b: Dnp3TcpBinding = serde_json::from_value(json!({
        "host": "relay", "port": 20000, "point_index": 8,
        "point_type": "analog_input", "scale": 1000.0,
    }))
    .unwrap();
    // Act + Assert
    assert_eq!(scaled(13.8, &b), 13_800.0);
}

#[test]
fn absent_scale_leaves_the_value_unchanged() {
    // Specs from before the field existed must read exactly as before.
    let b: Dnp3TcpBinding = serde_json::from_value(json!({
        "host": "relay", "port": 20000, "point_index": 0, "point_type": "analog_input",
    }))
    .unwrap();
    assert_eq!(scaled(412.5, &b), 412.5);
}
