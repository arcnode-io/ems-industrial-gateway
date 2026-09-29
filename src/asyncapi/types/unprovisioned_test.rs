//! An unprovisioned device (connection still PROVISIONED_AT_COMMISSIONING)
//! must be skipped, not fail the whole spec: devices come online one
//! commissioning POST at a time, and one missing address must not take
//! every other device down with it.

use crate::asyncapi::types::AsyncApiSpec;
use serde_json::{Value, json};

const SENTINEL: &str = "PROVISIONED_AT_COMMISSIONING";

fn modbus_entry(host: Value, port: Value) -> Value {
    json!({
        "unit": "watt_hours", "poll_rate_hz": 0.1, "protocol": "modbus_tcp",
        "host": host, "port": port, "unit_id": "1", "function_code": 3,
        "address": 4000, "data_type": "int32", "word_order": "high_low",
        "scale": 1.0, "offset": 0.0,
    })
}

fn command_entry(host: Value, port: Value) -> Value {
    json!({
        "verb": "set", "target": "active_power", "unit": "watts",
        "protocol": "modbus_tcp", "host": host, "port": port, "unit_id": "1",
        "address": 50, "scale": 1.0, "offset": 0.0,
    })
}

#[test]
fn unprovisioned_devices_are_skipped_and_the_rest_parse() {
    // Arrange — one commissioned meter, one not yet, same for a rack command
    let spec = json!({
        "info": { "version": "v1" },
        "x-protocol-source": {
            "meter_01": { "kwh_delivered": modbus_entry(json!("10.0.0.5"), json!(502)) },
            "meter_02": { "kwh_delivered": modbus_entry(json!(SENTINEL), json!(SENTINEL)) },
        },
        "x-command-source": {
            "rack_1": { "set_active_power": command_entry(json!("10.0.0.7"), json!(502)) },
            "rack_2": { "set_active_power": command_entry(json!(SENTINEL), json!(SENTINEL)) },
        },
    });
    // Act
    let parsed: AsyncApiSpec = serde_json::from_value(spec).unwrap();
    // Assert
    assert!(parsed.x_protocol_source.contains_key("meter_01"));
    assert!(!parsed.x_protocol_source.contains_key("meter_02"));
    assert!(parsed.x_command_source.contains_key("rack_1"));
    assert!(!parsed.x_command_source.contains_key("rack_2"));
}

#[test]
fn a_genuinely_malformed_entry_still_fails_loud() {
    // A string port that isn't the sentinel is a real error, not a skip.
    let spec = json!({
        "info": { "version": "v1" },
        "x-protocol-source": {
            "meter_01": { "kwh_delivered": modbus_entry(json!("10.0.0.5"), json!("five-oh-two")) },
        },
    });
    assert!(serde_json::from_value::<AsyncApiSpec>(spec).is_err());
}
