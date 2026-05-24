//! Wiremock stub for the gateway's `/asyncapi` endpoint. Lets each e2e test
//! ship a custom AsyncAPI body — including `x-device-trust` — without
//! standing up device-api + postgres.
//!
//! The stub itself is protocol-agnostic; per-protocol helpers build the
//! binding JSON the test wants to serve.

use serde_json::{Value, json};
use std::net::SocketAddr;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Spawn a wiremock that returns the given AsyncAPI body verbatim from
/// `GET /asyncapi`.
pub async fn spawn_asyncapi_stub(body: Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/asyncapi"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    server
}

/// Assemble an AsyncAPI body for a single (device_id, measurement) →
/// `protocol_binding`, with a `tls_mutual` trust entry validating
/// `subject_name`.
pub fn build_spec_body(
    device_id: &str,
    measurement: &str,
    unit: &str,
    protocol_binding: Value,
    subject_name: &str,
) -> Value {
    json!({
        "info": { "version": "v1" },
        "x-protocol-source": {
            device_id: {
                measurement: merge_meta(unit, 1.0, protocol_binding),
            }
        },
        "x-device-trust": {
            device_id: {
                "trust_mode": "tls_mutual",
                "subject_name": subject_name,
            }
        }
    })
}

/// Merge `unit` + `poll_rate_hz` into a binding object — what every
/// `x-protocol-source[device][measurement]` entry needs alongside its
/// protocol-specific fields.
fn merge_meta(unit: &str, poll_rate_hz: f64, mut binding: Value) -> Value {
    let map = binding
        .as_object_mut()
        .expect("binding must be a JSON object");
    map.insert("unit".into(), Value::String(unit.to_string()));
    map.insert(
        "poll_rate_hz".into(),
        serde_json::Number::from_f64(poll_rate_hz).unwrap().into(),
    );
    binding
}

/// Modbus/TLS binding JSON pointed at `addr`. Reads holding register 4000
/// with unit_id 1 (matches `modbus_security.rs` fixture).
pub fn modbus_tls_binding(addr: SocketAddr) -> Value {
    json!({
        "protocol": "modbus_tcp",
        "host": addr.ip().to_string(),
        "port": addr.port(),
        "unit_id": "1",
        "address": 4000,
        "scale": 1.0,
        "offset": 0.0,
    })
}

/// DNP3/TLS binding JSON pointed at `addr`. Reads AnalogInput at point_index 0
/// (matches `dnp3_security.rs` fixture).
pub fn dnp3_tls_binding(addr: SocketAddr) -> Value {
    json!({
        "protocol": "dnp3_tcp",
        "host": addr.ip().to_string(),
        "port": addr.port(),
        "point_index": 0,
        "point_type": "analog_input",
    })
}

/// Redfish HTTPS+mTLS binding JSON pointed at `addr`. Resource path + JSON
/// pointer match the `redfish_security.rs` fixture.
pub fn redfish_tls_binding(addr: SocketAddr, resource_path: &str, json_pointer: &str) -> Value {
    json!({
        "protocol": "redfish",
        "host": addr.ip().to_string(),
        "port": addr.port(),
        "uri": resource_path,
        "json_pointer": json_pointer,
    })
}
