//! Boot orchestration. Tier 1: one-shot fetch → per-device protocol read → MQTT publish → exit.

use crate::asyncapi::types::{AsyncApiSpec, ProtocolBinding};
use crate::bacnet::client as bacnet;
use crate::config::Config;
use crate::dnp3::client as dnp3;
use crate::http::client::fetch_asyncapi;
use crate::modbus::client as modbus;
use crate::mqtt::{publisher, subscriber};
use crate::redfish::client as redfish;
use crate::snmp::client as snmp;
use anyhow::{Context, Result};
use tracing::info;

/// A measurement the gateway will read + publish each tick. Until Tier 2
/// walks /asyncapi for every channel, we hardcode the few we care about.
struct Target {
    /// Device slug (matches the DTM key).
    device_id: &'static str,
    /// Measurement name on the device's template.
    measurement: &'static str,
    /// Engineering unit, used as the terminal MQTT topic segment.
    unit: &'static str,
}

/// Hardcoded Tier 1 measurements.
const TARGETS: &[Target] = &[
    Target {
        device_id: "meter_01",
        measurement: "kwh_delivered",
        unit: "watt_hours",
    },
    Target {
        device_id: "pdu_01",
        measurement: "input_current",
        unit: "amps",
    },
    Target {
        device_id: "switch_01",
        measurement: "inlet_temp",
        unit: "celsius",
    },
    Target {
        device_id: "relay_01",
        measurement: "phase_a_current",
        unit: "amps",
    },
    Target {
        device_id: "cooler_01",
        measurement: "supply_water_temp",
        unit: "celsius",
    },
];

/// Tier 1 flow: read every target, publish each to MQTT, exit.
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

    for target in TARGETS {
        read_and_publish(&spec, &cfg, &client, target).await?;
    }

    client.disconnect(None).await.context("mqtt disconnect")?;
    Ok(())
}

/// Look up a binding, dispatch to the right protocol client, publish.
async fn read_and_publish(
    spec: &AsyncApiSpec,
    cfg: &Config,
    client: &paho_mqtt::AsyncClient,
    target: &Target,
) -> Result<()> {
    let binding = spec
        .x_protocol_source
        .get(target.device_id)
        .and_then(|m| m.get(target.measurement))
        .with_context(|| {
            format!(
                "x-protocol-source missing {}.{}",
                target.device_id, target.measurement
            )
        })?;

    let value = read_value(binding).await?;
    info!(target.device_id, target.measurement, value, "read complete");

    let topic = format!(
        "sites/{}/devices/{}/measurements/{}/{}",
        cfg.site_id, target.device_id, target.measurement, target.unit,
    );
    publisher::publish_measurement(client, &topic, value).await?;
    info!(%topic, "published");
    Ok(())
}

/// Single-point protocol dispatch. Add a `match` arm when a new
/// `ProtocolBinding` variant lands.
async fn read_value(binding: &ProtocolBinding) -> Result<f64> {
    match binding {
        ProtocolBinding::ModbusTcp(b) => modbus::read_measurement(b).await,
        ProtocolBinding::Snmp(b) => snmp::read_measurement(b).await,
        ProtocolBinding::Redfish(b) => redfish::read_measurement(b).await,
        ProtocolBinding::Dnp3Tcp(b) => dnp3::read_measurement(b).await,
        ProtocolBinding::BacnetIp(b) => bacnet::read_measurement(b).await,
    }
}
