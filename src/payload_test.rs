//! Payload shaping. High-risk: a reading published in the wrong type is
//! misread by every consumer (an enum code read as a number, a breaker
//! state read as 1.0), and one outside its schema must never be published.

use super::{Payload, Raw};
use serde_json::{Value, json};
use std::collections::HashMap;

const TS: &str = "2026-10-01T00:00:00Z";

fn sample_schema(value: Value) -> Value {
    json!({
        "type": "object", "required": ["ts", "value"],
        "properties": { "ts": { "type": "string", "format": "date-time" }, "value": value },
    })
}

fn throttle_reason() -> Payload {
    Payload::compile(&sample_schema(
        json!({ "type": "string", "enum": ["NA", "SW_POWER_CAP"] }),
    ))
    .unwrap()
}

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn a_number_publishes_as_a_number() {
    let p = Payload::compile(&sample_schema(json!({ "type": "number" }))).unwrap();
    assert_eq!(
        p.sample(&Raw::Number(42.5), None, TS).unwrap(),
        json!({ "ts": TS, "value": 42.5 })
    );
}

#[test]
fn a_boolean_point_publishes_true_or_false() {
    let p = Payload::compile(&sample_schema(json!({ "type": "boolean" }))).unwrap();
    assert_eq!(
        p.sample(&Raw::Number(1.0), None, TS).unwrap()["value"],
        json!(true)
    );
    assert_eq!(
        p.sample(&Raw::Number(0.0), None, TS).unwrap()["value"],
        json!(false)
    );
}

#[test]
fn vendor_text_publishes_as_our_label() {
    let m = map(&[("SWPowerCap", "SW_POWER_CAP")]);
    let sample = throttle_reason()
        .sample(&Raw::Text("SWPowerCap".into()), Some(&m), TS)
        .unwrap();
    assert_eq!(sample["value"], json!("SW_POWER_CAP"));
}

#[test]
fn a_register_code_publishes_as_our_label() {
    let p = Payload::compile(&sample_schema(
        json!({ "type": "string", "enum": ["AUTOMATIC_INTERNAL", "AUTOMATIC_EXTERNAL_BUS"] }),
    ))
    .unwrap();
    let m = map(&[("0", "AUTOMATIC_INTERNAL"), ("2", "AUTOMATIC_EXTERNAL_BUS")]);
    assert_eq!(
        p.sample(&Raw::Number(2.0), Some(&m), TS).unwrap()["value"],
        json!("AUTOMATIC_EXTERNAL_BUS")
    );
}

#[test]
fn a_label_outside_the_schema_is_never_published() {
    // The map names a label the schema doesn't allow
    let m = map(&[("Quiesced", "QUIESCED")]);
    assert!(
        throttle_reason()
            .sample(&Raw::Text("Quiesced".into()), Some(&m), TS)
            .is_err()
    );
}

#[test]
fn an_unmapped_reading_for_an_enum_is_an_error() {
    let m = map(&[("SWPowerCap", "SW_POWER_CAP")]);
    assert!(
        throttle_reason()
            .sample(&Raw::Text("HWSlowdown".into()), Some(&m), TS)
            .is_err()
    );
}

#[test]
fn text_for_a_number_is_an_error() {
    let p = Payload::compile(&sample_schema(json!({ "type": "number" }))).unwrap();
    assert!(p.sample(&Raw::Text("Enabled".into()), None, TS).is_err());
}

#[test]
fn a_payload_reference_resolves_to_its_compiled_schema() {
    // Arrange — the spec's components and an entry's reference into them
    let schemas = HashMap::from([(
        "ProtectiveRelay_BreakerClosed".to_string(),
        sample_schema(json!({ "type": "boolean" })),
    )]);
    let reference = json!({ "$ref": "#/components/schemas/ProtectiveRelay_BreakerClosed" });
    // Act
    let p = super::resolve(&reference, &schemas).unwrap();
    // Assert
    assert_eq!(
        p.sample(&Raw::Number(1.0), None, TS).unwrap()["value"],
        json!(true)
    );
}

#[test]
fn a_reference_to_a_missing_schema_is_an_error() {
    let reference = json!({ "$ref": "#/components/schemas/Nope" });
    assert!(super::resolve(&reference, &HashMap::new()).is_err());
}
