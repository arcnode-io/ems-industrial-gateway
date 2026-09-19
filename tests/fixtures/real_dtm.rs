//! Builds a real DTM POST body for `bess_module_1` + 2 `bess_rack` children +
//! `operating_envelope`, using the real edp-api templates transcribed into
//! `bess_templates.json` (edp-api@0fe1856). Shared by any e2e test that
//! needs a real device-api container resolving real production template
//! content, rather than a hand-rolled AsyncAPI stub.

use serde_json::{Map, Value, json};

pub const MODULE_ID: &str = "bess_module_1";
pub const RACK_1: &str = "rack_1";
pub const RACK_2: &str = "rack_2";

fn device(
    device_id: &str,
    template: &str,
    parent: Option<&str>,
    host: Option<(&str, u16)>,
) -> Value {
    json!({
        "device_id": device_id,
        "template": template,
        "parent": parent,
        "connection": host.map(|(h, p)| json!({ "host": h, "port": p, "unit_id": "1" })),
        "blocking": [],
    })
}

/// Full `/topology` POST body: `bess_module_1` (no connection, aggregation
/// only) + `rack_1`/`rack_2` (real Modbus connections) + `operating_envelope`
/// (dummy connection — device-api's envelope-guard resolution requires the
/// device to exist and have a host/port for its dnp3_tcp binding to
/// deserialize on the gateway side, but no outstation needs to answer since
/// import/export limit are driven by direct MQTT publish in the test).
pub fn bess_dtm(rack1_port: u16, rack2_port: u16) -> Value {
    let mut devices = Map::new();
    devices.insert(
        MODULE_ID.to_string(),
        device(MODULE_ID, "bess_module", None, None),
    );
    devices.insert(
        RACK_1.to_string(),
        device(
            RACK_1,
            "bess_rack",
            Some(MODULE_ID),
            Some(("127.0.0.1", rack1_port)),
        ),
    );
    devices.insert(
        RACK_2.to_string(),
        device(
            RACK_2,
            "bess_rack",
            Some(MODULE_ID),
            Some(("127.0.0.1", rack2_port)),
        ),
    );
    devices.insert(
        "operating_envelope".to_string(),
        device(
            "operating_envelope",
            "operating_envelope",
            None,
            Some(("127.0.0.1", 20000)),
        ),
    );

    let templates: Value = serde_json::from_str(include_str!("bess_templates.json"))
        .expect("bess_templates.json parses");
    json!({
        "deployment_uuid": "22222222-2222-4222-8222-222222222222",
        "sizing_params": { "P_compute_total_kW": 100, "E_BESS_total_kWh": 8000, "T_coolant_setpoint_C": 18 },
        "devices": devices,
        "buses": [],
        "templates_used": templates,
    })
}

/// Seed a rack's SoC (raw register, scale 0.1 -> percent) and operating_state
/// (0 = STANDBY) via the mock server's HTTP control surface, so the gateway's
/// real Modbus poll picks up real, differentiated values.
pub async fn seed_rack(control_port: u16, soc_raw: u16) -> anyhow::Result<()> {
    reqwest::Client::new()
        .put(format!("http://127.0.0.1:{control_port}/registers"))
        // bess_rack.yaml: state_of_charge @ addr 0, operating_state @ addr 40.
        .json(&json!({ "registers": { "0": soc_raw, "40": 0 } }))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}
