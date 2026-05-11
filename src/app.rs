//! Boot orchestration. Tier 1: one-shot fetch → modbus read → MQTT publish → exit.

use crate::config::Config;
use crate::http::client::fetch_asyncapi;
use crate::modbus::client::{apply_scale_offset, decode_int32, read_holding, WordOrder};
use crate::mqtt::{publisher, subscriber};
use anyhow::{Context, Result};
use tracing::info;

const DEVICE_ID: &str = "meter_01";
const MEASUREMENT: &str = "kwh_delivered";
const UNIT: &str = "watt_hours";

/// Tier 1 flow: read one meter_01 register, publish one FloatSample, exit.
pub async fn run(cfg: Config) -> Result<()> {
    info!(
        device_api_url = %cfg.device_api_url,
        broker_url = %cfg.broker_url,
        site_id = %cfg.site_id,
        "gateway starting",
    );

    // Connect MQTT first so the subscriber catches early beacons.
    let mut client = publisher::connect(&cfg.broker_url, "ems-industrial-gateway").await?;
    subscriber::subscribe_topology_changed(&mut client).await?;

    // Fetch + validate the spec.
    let spec = fetch_asyncapi(&cfg.device_api_url).await?;
    info!(version = %spec.info.version, "spec fetched");

    // Pull binding metadata for meter_01.kwh_delivered out of x-protocol-source.
    let binding = spec
        .x_protocol_source
        .get(DEVICE_ID)
        .and_then(|m| m.get(MEASUREMENT))
        .context("x-protocol-source missing meter_01.kwh_delivered")?;

    // int32 = 2× u16 registers.
    let words = read_holding(&binding.host, binding.port, binding.unit_id, binding.address, 2).await?;
    let raw = decode_int32(&words, WordOrder::HighLow);
    let value = apply_scale_offset(raw, binding.scale, binding.offset);
    info!(raw, value, "modbus read complete");

    let topic = format!(
        "sites/{}/devices/{}/measurements/{}/{}",
        cfg.site_id, DEVICE_ID, MEASUREMENT, UNIT,
    );
    publisher::publish_measurement(&client, &topic, value).await?;
    info!(%topic, "published");

    client.disconnect(None).await.context("mqtt disconnect")?;
    Ok(())
}
