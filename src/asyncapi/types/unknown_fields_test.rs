//! A spec entry carrying a field the gateway doesn't know fails the whole
//! spec, naming the field. A typo or a template field the gateway doesn't
//! implement must not be silently ignored and run on defaults.

use crate::asyncapi::types::AsyncApiSpec;
use serde_json::{Value, json};

fn measurement() -> Value {
    json!({
        "unit": "watts", "poll_rate_hz": 1.0, "protocol": "modbus_tcp",
        "host": "10.0.0.5", "port": 502, "unit_id": "1", "function_code": 3,
        "address": 40, "data_type": "uint16", "word_order": "high_low",
        "scale": 1.0, "offset": 0.0,
    })
}

fn distribute_command() -> Value {
    json!({
        "verb": "set", "target": "active_power", "unit": "watts",
        "protocol": "distribute", "allocation_policy": "equal_split",
        "children": [{
            "device_id": "rack_1",
            "operating_state_topic": "sites/{site_id}/devices/rack_1/measurements/operating_state/none",
            "state_of_charge_topic": "sites/{site_id}/devices/rack_1/measurements/state_of_charge/percent",
            "power_min": -1.0, "power_max": 1.0,
        }],
    })
}

fn synthetic_measurement() -> Value {
    json!({
        "unit": "percent", "poll_rate_hz": 1.0, "protocol": "synthetic",
        "operation": "weighted_mean",
        "pairs": [{ "topic": "sites/{site_id}/devices/r/measurements/s/percent", "weight": 1.0 }],
    })
}

/// Parse a spec holding one measurement and one command, return the error.
fn parse_error(measurement: Value, command: Value) -> String {
    let spec = json!({
        "info": { "version": "v1" },
        "x-protocol-source": { "dev": { "m": measurement } },
        "x-command-source": { "mod": { "set_active_power": command } },
    });
    match serde_json::from_value::<AsyncApiSpec>(spec) {
        Ok(_) => "parsed".to_string(),
        Err(e) => e.to_string(),
    }
}

#[test]
fn the_fixtures_parse_as_they_are() {
    assert_eq!(parse_error(measurement(), distribute_command()), "parsed");
    assert_eq!(
        parse_error(synthetic_measurement(), distribute_command()),
        "parsed"
    );
}

#[test]
fn a_misspelled_binding_field_fails_the_spec() {
    let mut m = measurement();
    m["adress"] = json!(41);
    assert!(parse_error(m, distribute_command()).contains("adress"));
}

#[test]
fn an_unknown_channel_field_fails_the_spec() {
    let mut m = measurement();
    m["endianness"] = json!("little");
    assert!(parse_error(m, distribute_command()).contains("endianness"));
}

#[test]
fn an_unknown_command_field_fails_the_spec() {
    let mut c = distribute_command();
    c["allocation_mode"] = json!("fast");
    assert!(parse_error(measurement(), c).contains("allocation_mode"));
}

#[test]
fn an_unknown_field_in_a_nested_entry_fails_the_spec() {
    let mut c = distribute_command();
    c["children"][0]["capacity"] = json!(4000);
    assert!(parse_error(measurement(), c).contains("capacity"));
    let mut s = synthetic_measurement();
    s["pairs"][0]["unit"] = json!("kwh");
    assert!(parse_error(s, distribute_command()).contains("unit"));
}

#[test]
fn a_distribute_binding_with_a_charging_obligation_parses() {
    // Arrange — readiness 91.8% SoC, recharge 248.2 kW (ERCOT heavy preset)
    let mut c = distribute_command();
    c["readiness_soc_percent"] = json!(91.8);
    c["recharge_power_w"] = json!(248_200.0);
    // Act + Assert
    assert_eq!(parse_error(measurement(), c), "parsed");
}
