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

#[test]
fn a_command_with_a_sunspec_scale_factor_is_skipped() {
    // Writes don't apply sunssf; writing one unscaled would be off by 10^sf
    let mut command = fc_command(16, "int16");
    command["scale_factor_address"] = json!(40085);
    let spec = json!({
        "info": { "version": "v1" },
        "x-protocol-source": {},
        "x-command-source": { "inverter": { "set_active_power": command } },
    });
    let parsed: AsyncApiSpec = serde_json::from_value(spec).unwrap();
    assert!(!parsed.x_command_source.contains_key("inverter"));
}

#[test]
fn a_dnp3_measurement_with_an_unread_point_type_is_skipped() {
    // Counters and outputs aren't read; better skipped at parse than failing every poll
    let entry = json!({
        "unit": "none", "poll_rate_hz": 1, "protocol": "dnp3_tcp",
        "host": "10.0.0.20", "port": 20000, "point_index": 3, "point_type": "counter",
    });
    let spec = json!({
        "info": { "version": "v1" },
        "x-protocol-source": { "relay": { "trip_count": entry } },
    });
    let parsed: AsyncApiSpec = serde_json::from_value(spec).unwrap();
    assert!(!parsed.x_protocol_source.contains_key("relay"));
}

#[test]
fn a_power_cap_with_half_an_envelope_guard_is_skipped_not_run_unguarded() {
    // Arrange — topics present, hysteresis missing: shedding would silently
    // be off on a site that enabled it
    let child = json!({ "device_id": "gpu_node_01", "target": "gpu_1_power_limit", "min_w": 200.0, "max_w": 1000.0 });
    let cap = |guard: Value| {
        let mut c = json!({
            "verb": "set", "target": "power_limit", "unit": "percent",
            "protocol": "power_cap", "children": [child.clone()],
        });
        c.as_object_mut()
            .unwrap()
            .extend(guard.as_object().unwrap().clone());
        c
    };
    let topics = json!({
        "import_limit_topic": "i", "export_limit_topic": "e", "poi_active_power_topic": "p",
    });
    let mut full = topics.clone();
    full.as_object_mut().unwrap().extend(
        json!({ "hysteresis_margin": 0.05, "hysteresis_dwell_secs": 30.0, "ramp_rate_per_sec": 0.1 })
            .as_object()
            .unwrap()
            .clone(),
    );
    let spec = json!({
        "info": { "version": "v1" },
        "x-protocol-source": {},
        "x-command-source": {
            "compute_plain": { "set_power_limit": cap(json!({})) },
            "compute_guarded": { "set_power_limit": cap(full) },
            "compute_half": { "set_power_limit": cap(topics) },
        },
    });
    // Act
    let parsed: AsyncApiSpec = serde_json::from_value(spec).unwrap();
    // Assert
    assert!(parsed.x_command_source.contains_key("compute_plain"));
    assert!(parsed.x_command_source.contains_key("compute_guarded"));
    assert!(!parsed.x_command_source.contains_key("compute_half"));
}
