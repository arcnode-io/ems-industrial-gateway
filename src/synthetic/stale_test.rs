//! A polled input is dead after 3 missed polls, never under 5 s; an input
//! the gateway doesn't poll is never called dead here.

use super::stale_limits;
use crate::asyncapi::types::AsyncApiSpec;
use serde_json::json;
use std::time::Duration;

/// A spec with `pdu.input_power` at 1 Hz, `meter.energy` at 0.1 Hz and
/// `relay.voltage` at the default rate.
fn spec() -> AsyncApiSpec {
    let at = |hz: f64| {
        json!({
            "unit": "watts", "poll_rate_hz": hz, "protocol": "modbus_tcp",
            "host": "10.0.0.5", "port": 502, "unit_id": "1", "function_code": 3,
            "address": 40, "data_type": "uint16", "word_order": "high_low",
            "scale": 1.0, "offset": 0.0,
        })
    };
    let mut unrated = at(1.0);
    unrated.as_object_mut().unwrap().remove("poll_rate_hz");
    serde_json::from_value(json!({
        "info": { "version": "v1" },
        "x-protocol-source": {
            "pdu": { "input_power": at(1.0) },
            "meter": { "energy": at(0.1) },
            "relay": { "voltage": unrated },
        },
    }))
    .unwrap()
}

const PDU: &str = "sites/s/devices/pdu/measurements/input_power/watts";
const METER: &str = "sites/s/devices/meter/measurements/energy/watts";

#[test]
fn fast_inputs_get_the_five_second_floor() {
    let limits = stale_limits(&spec(), &[PDU.into()]);
    assert_eq!(limits[PDU], Duration::from_secs(5));
}

#[test]
fn a_slow_input_waits_three_of_its_polls() {
    // Act — 0.1 Hz: three missed 10 s polls; the 1 Hz input keeps its own
    let limits = stale_limits(&spec(), &[PDU.into(), METER.into()]);
    // Assert
    assert_eq!(limits[METER], Duration::from_secs(30));
    assert_eq!(limits[PDU], Duration::from_secs(5));
}

#[test]
fn an_input_the_gateway_doesnt_poll_never_goes_stale() {
    let other = "sites/s/devices/operating_envelope/measurements/import_limit/watts";
    assert!(stale_limits(&spec(), &[other.into()]).is_empty());
}

#[test]
fn an_input_without_a_rate_waits_on_the_default_poll() {
    let relay = "sites/s/devices/relay/measurements/voltage/watts";
    assert_eq!(
        stale_limits(&spec(), &[relay.into()])[relay],
        Duration::from_secs(5)
    );
}
