//! Source-map parsing: an unprovisioned device (connection still
//! PROVISIONED_AT_COMMISSIONING) or one with an unusable Modbus function code
//! is skipped, not fatal to the whole spec. One bad device must not take
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

fn fc_entry(function_code: u8, data_type: &str) -> Value {
    json!({
        "unit": "celsius", "poll_rate_hz": 1, "protocol": "modbus_tcp",
        "host": "10.0.0.9", "port": 502, "unit_id": "1", "address": 53508,
        "scale": 0.1, "offset": 0.0, "data_type": data_type, "word_order": "high_low",
        "function_code": function_code,
    })
}

fn fc_command(function_code: u8, data_type: &str) -> Value {
    json!({
        "verb": "set", "target": "leaving_fluid_temp", "unit": "celsius",
        "protocol": "modbus_tcp", "host": "10.0.0.9", "port": 502, "unit_id": "1",
        "address": 53257, "scale": 0.1, "offset": 0.0, "data_type": data_type,
        "word_order": "high_low", "function_code": function_code,
    })
}

#[test]
fn valid_function_codes_are_kept() {
    let spec = json!({
        "info": { "version": "v1" },
        "x-protocol-source": { "cooler": { "leaving_fluid_temp": fc_entry(4, "int16") } },
        "x-command-source": { "cooler": { "set_leaving_fluid_temp": fc_command(6, "uint16") } },
    });
    let parsed: AsyncApiSpec = serde_json::from_value(spec).unwrap();
    assert!(parsed.x_protocol_source.contains_key("cooler"));
    assert!(parsed.x_command_source.contains_key("cooler"));
}

#[test]
fn a_measurement_with_a_write_code_is_skipped_not_fatal() {
    let spec = json!({
        "info": { "version": "v1" },
        "x-protocol-source": {
            "cooler": { "leaving_fluid_temp": fc_entry(16, "int16") },
            "meter_01": { "kwh_delivered": modbus_entry(json!("10.0.0.5"), json!(502)) },
        },
    });
    let parsed: AsyncApiSpec = serde_json::from_value(spec).unwrap();
    assert!(!parsed.x_protocol_source.contains_key("cooler"));
    assert!(parsed.x_protocol_source.contains_key("meter_01"));
}

#[test]
fn an_fc6_command_on_a_multi_register_type_is_skipped() {
    let spec = json!({
        "info": { "version": "v1" },
        "x-protocol-source": {},
        "x-command-source": { "cooler": { "set_leaving_fluid_temp": fc_command(6, "int32") } },
    });
    let parsed: AsyncApiSpec = serde_json::from_value(spec).unwrap();
    assert!(!parsed.x_command_source.contains_key("cooler"));
}
