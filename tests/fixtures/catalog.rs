//! The template catalog device-api ships with, and a DTM that instantiates
//! every template in it once.
//!
//! The catalog comes out of the device-api container itself, not a copy:
//! that image is what reaches the customer, templates baked in.

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use testcontainers::core::ExecCommand;
use testcontainers::{ContainerAsync, GenericImage};

/// Where device-api's image keeps the catalog (cfg.yml `templateCatalogRoot`).
const CATALOG_ROOT: &str = "/app/device_templates";

/// Every template in the running device-api's catalog, keyed by slug.
pub async fn shipped_catalog(
    device_api: &ContainerAsync<GenericImage>,
) -> Result<Map<String, Value>> {
    // One YAML stream, one document per template file.
    let script = format!("for f in {CATALOG_ROOT}/*/*.yaml; do echo ---; cat \"$f\"; done");
    let mut out = device_api
        .exec(ExecCommand::new(["sh", "-c", &script]))
        .await?;
    let yaml = String::from_utf8(out.stdout_to_vec().await?)?;
    let catalog: Map<String, Value> = serde_yaml::Deserializer::from_str(&yaml)
        .map(|doc| {
            let t = Value::deserialize(doc)?;
            let slug = t["template"].as_str().context("template without a slug")?;
            Ok((slug.to_string(), t))
        })
        .collect::<Result<_>>()?;
    ensure!(!catalog.is_empty(), "no templates under {CATALOG_ROOT}");
    Ok(catalog)
}

/// DTM with one device per template, device_id = slug. Leaves get a
/// connection (nothing needs to answer on it); modules are pure rollups.
pub fn one_of_each(catalog: &Map<String, Value>) -> Value {
    let devices: Map<String, Value> = catalog
        .iter()
        .enumerate()
        .map(|(i, (slug, t))| {
            let connection = (t["kind"] == "leaf")
                .then(|| json!({ "host": "127.0.0.1", "port": 20000 + i, "unit_id": "1" }));
            let device = json!({ "device_id": slug, "template": slug, "connection": connection });
            (slug.clone(), device)
        })
        .collect();
    json!({
        "deployment_uuid": "33333333-3333-4333-8333-333333333333",
        "sizing_params": { "P_compute_total_kW": 100, "E_BESS_total_kWh": 8000, "T_coolant_setpoint_C": 18 },
        "devices": devices,
        "buses": [],
        "templates_used": catalog,
    })
}
