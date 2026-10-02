//! Shape a raw reading into the payload its measurement's schema declares,
//! and check it against that schema before it's published.
//!
//! A measurement is a number, a boolean or an enum label in our own
//! vocabulary (`"SW_POWER_CAP"`), as device-api's AsyncAPI declares per
//! measurement. A reading that doesn't fit its schema is never published.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::HashMap;

/// A reading as the device gave it.
#[derive(Debug, Clone, PartialEq)]
pub enum Raw {
    /// A numeric reading (registers, points, sensors).
    Number(f64),
    /// A text reading (e.g. Redfish `Status/State`).
    Text(String),
}

/// What a measurement's schema says its `value` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `"type": "number"`
    Number,
    /// `"type": "boolean"`
    Boolean,
    /// `"type": "string"` with an `enum` of labels
    Label,
}

/// A measurement's compiled payload schema.
#[derive(Debug)]
pub struct Payload {
    /// What `value` must be.
    kind: Kind,
    /// The whole sample schema (`{ts, value}`), for the final check.
    validator: jsonschema::Validator,
}

impl Payload {
    /// Compile a sample schema (`{ts, value}`) from the spec.
    pub fn compile(schema: &Value) -> Result<Self> {
        let kind = match schema
            .pointer("/properties/value/type")
            .and_then(Value::as_str)
        {
            Some("number" | "integer") => Kind::Number,
            Some("boolean") => Kind::Boolean,
            Some("string") => Kind::Label,
            other => bail!("measurement schema has no number/boolean/string value: {other:?}"),
        };
        let validator = jsonschema::validator_for(schema)
            .map_err(|e| anyhow!("invalid measurement schema: {e}"))?;
        Ok(Self { kind, validator })
    }

    /// The `{ts, value}` sample for `raw`, or an error if it can't be shaped
    /// or doesn't fit the schema. `value_map` maps a raw value (text, or a
    /// number written as text) to its label.
    pub fn sample(
        &self,
        raw: &Raw,
        value_map: Option<&HashMap<String, String>>,
        ts: &str,
    ) -> Result<Value> {
        let value = match (self.kind, raw) {
            (Kind::Number, Raw::Number(n)) => json!(n),
            (Kind::Boolean, Raw::Number(n)) => json!(*n != 0.0),
            (Kind::Label, raw) => {
                let key = match raw {
                    Raw::Text(text) => text.clone(),
                    // Register codes are keyed as written: 2, not 2.0.
                    Raw::Number(n) if n.fract() == 0.0 => format!("{}", *n as i64),
                    Raw::Number(n) => n.to_string(),
                };
                let label = value_map
                    .and_then(|m| m.get(&key))
                    .with_context(|| format!("reading {key:?} has no label in the value_map"))?;
                json!(label)
            }
            (kind, raw) => bail!("a {raw:?} reading can't be a {kind:?} measurement"),
        };
        let sample = json!({ "ts": ts, "value": value });
        self.validator
            .validate(&sample)
            .map_err(|e| anyhow!("{sample} doesn't fit its schema: {e}"))?;
        Ok(sample)
    }
}

/// Compile the schema an entry's `{"$ref": "#/components/schemas/<name>"}`
/// points at in the spec's `schemas`.
pub fn resolve(reference: &Value, schemas: &HashMap<String, Value>) -> Result<Payload> {
    let target = reference
        .get("$ref")
        .and_then(Value::as_str)
        .with_context(|| format!("payload {reference} is not a $ref"))?;
    let name = target
        .strip_prefix("#/components/schemas/")
        .with_context(|| format!("payload $ref {target} is not into components.schemas"))?;
    let schema = schemas
        .get(name)
        .with_context(|| format!("payload $ref {target} names no schema in the spec"))?;
    Payload::compile(schema)
}

#[cfg(test)]
#[path = "payload_test.rs"]
mod tests;
