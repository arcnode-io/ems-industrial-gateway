//! Boot orchestration. Tier 1: one-shot fetch → per-device protocol read → MQTT publish → exit.

use crate::asyncapi::types::{AsyncApiSpec, ProtocolBinding};
use crate::config::Config;
use crate::http::client::fetch_asyncapi;
use crate::modbus::client::{WordOrder, apply_scale_offset, decode_int32, read_holding};
use crate::mqtt::{publisher, subscriber};
use crate::snmp::client::read_integer;
use anyhow::{Context, Result};
use tracing::info;

/// Tier 1 / Modbus device id (matches the seeded DTM).
const MODBUS_DEVICE_ID: &str = "meter_01";
/// Modbus measurement channel.
const MODBUS_MEASUREMENT: &str = "kwh_delivered";
/// Engineering unit of the Modbus measurement.
const MODBUS_UNIT: &str = "watt_hours";

/// Tier 1 / SNMP device id (matches the seeded DTM).
const SNMP_DEVICE_ID: &str = "pdu_01";
/// SNMP measurement channel.
const SNMP_MEASUREMENT: &str = "input_current";
/// Engineering unit of the SNMP measurement.
const SNMP_UNIT: &str = "amps";

/// Tier 1 flow: read both devices, publish each to MQTT, exit.
pub async fn run(cfg: Config) -> Result<()> {
    info!(
        device_api_url = %cfg.device_api_url,
        broker_url = %cfg.broker_url,
        site_id = %cfg.site_id,
        "gateway starting",
    );

    let mut client = publisher::connect(&cfg.broker_url, "ems-industrial-gateway").await?;
    subscriber::subscribe_topology_changed(&mut client).await?;

    let spec = fetch_asyncapi(&cfg.device_api_url).await?;
    info!(version = %spec.info.version, "spec fetched");

    read_and_publish_modbus(&spec, &cfg, &client).await?;
    read_and_publish_snmp(&spec, &cfg, &client).await?;

    client.disconnect(None).await.context("mqtt disconnect")?;
    Ok(())
}

/// Read meter_01.kwh_delivered (Modbus) and publish as a FloatSample.
async fn read_and_publish_modbus(
    spec: &AsyncApiSpec,
    cfg: &Config,
    client: &paho_mqtt::AsyncClient,
) -> Result<()> {
    let binding = spec
        .x_protocol_source
        .get(MODBUS_DEVICE_ID)
        .and_then(|m| m.get(MODBUS_MEASUREMENT))
        .context("x-protocol-source missing meter_01.kwh_delivered")?;
    let ProtocolBinding::ModbusTcp(b) = binding else {
        anyhow::bail!("expected Modbus binding for meter_01.kwh_delivered");
    };
    let unit_id: u8 = b
        .unit_id
        .parse()
        .context("unit_id must parse to u8 for Modbus")?;
    let words = read_holding(&b.host, b.port, unit_id, b.address, 2).await?;
    let raw = decode_int32(&words, WordOrder::HighLow);
    let value = apply_scale_offset(raw, b.scale, b.offset);
    info!(raw, value, "modbus read complete");

    let topic = format!(
        "sites/{}/devices/{}/measurements/{}/{}",
        cfg.site_id, MODBUS_DEVICE_ID, MODBUS_MEASUREMENT, MODBUS_UNIT,
    );
    publisher::publish_measurement(client, &topic, value).await?;
    info!(%topic, "modbus published");
    Ok(())
}

/// Read pdu_01.input_current (SNMP) and publish as a FloatSample.
async fn read_and_publish_snmp(
    spec: &AsyncApiSpec,
    cfg: &Config,
    client: &paho_mqtt::AsyncClient,
) -> Result<()> {
    let binding = spec
        .x_protocol_source
        .get(SNMP_DEVICE_ID)
        .and_then(|m| m.get(SNMP_MEASUREMENT))
        .context("x-protocol-source missing pdu_01.input_current")?;
    let ProtocolBinding::Snmp(b) = binding else {
        anyhow::bail!("expected SNMP binding for pdu_01.input_current");
    };
    let raw = read_integer(&b.host, b.port, &b.oid).await?;
    let value = raw as f64;
    info!(raw, value, "snmp read complete");

    let topic = format!(
        "sites/{}/devices/{}/measurements/{}/{}",
        cfg.site_id, SNMP_DEVICE_ID, SNMP_MEASUREMENT, SNMP_UNIT,
    );
    publisher::publish_measurement(client, &topic, value).await?;
    info!(%topic, "snmp published");
    Ok(())
}
