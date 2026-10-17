//! e2e: only a command that lands becomes the operator's requested
//! setpoint. The envelope loop and the site-distribution release steer
//! toward that record, so a command acked `failed` must leave it as it was;
//! otherwise a refused setpoint gets applied a tick later anyway.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::asyncapi::types::ProtocolBinding;
use ems_industrial_gateway::dispatch::handle_command;
use ems_industrial_gateway::synthetic::new_input_cache;
use fixtures::containers::{start_hivemq, unique_network};
use fixtures::sunspec_pcs::{Pcs, at, spawn};
use paho_mqtt::AsyncClient;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use tokio::sync::RwLock;

const SITE: &str = "local_site";
const PCS: &str = "pcs_1";
const KEY: &str = "set_active_power";

type Requested = RwLock<HashMap<String, HashMap<String, f64>>>;

fn channels(pcs: &Pcs) -> HashMap<String, HashMap<String, ProtocolBinding>> {
    let binding = serde_json::from_value(json!({
        "protocol": "sunspec", "host": pcs.addr.ip().to_string(), "port": pcs.addr.port(),
        "unit_id": "1", "model_id": 65534, "point": "CmdRealPwr",
    }))
    .unwrap();
    HashMap::from([(PCS.to_string(), HashMap::from([(KEY.to_string(), binding)]))])
}

/// Send `pcs_1` a `value` W command through the dispatch handler.
async fn command(client: &AsyncClient, pcs: &Pcs, requested: &Requested, value: f64) -> Result<()> {
    let topic = format!("sites/{SITE}/devices/{PCS}/commands/set/active_power/watts");
    let payload = json!({ "ts": "t", "value": value, "command_id": "c" }).to_string();
    handle_command(
        client,
        SITE,
        &channels(pcs),
        &HashMap::new(),
        &HashSet::new(),
        None,
        &new_input_cache(),
        requested,
        &topic,
        payload.as_bytes(),
    )
    .await
}

async fn requested_now(requested: &Requested) -> Option<f64> {
    requested
        .read()
        .await
        .get(PCS)
        .and_then(|m| m.get(KEY))
        .copied()
}

#[tokio::test]
async fn a_refused_setpoint_leaves_the_last_good_request_in_place() -> Result<()> {
    // Arrange — PWR_SF = -3 (milliwatts): 5 MW doesn't fit an int32
    let hivemq = start_hivemq(&unique_network()).await?;
    let client = AsyncClient::new(format!(
        "tcp://localhost:{}",
        hivemq.get_host_port_ipv4(1883).await?
    ))?;
    client.connect(None).await?;
    let pcs = spawn(40000).await?;
    pcs.set(at::PWR_SF, (-3i16) as u16);
    let requested = Requested::default();

    // Act + Assert — a write that lands is recorded
    command(&client, &pcs, &requested, 100_000.0).await?;
    assert_eq!(requested_now(&requested).await, Some(100_000.0));

    // Act + Assert — one that's refused isn't
    command(&client, &pcs, &requested, 5e6).await?;
    assert_eq!(requested_now(&requested).await, Some(100_000.0));
    Ok(())
}

#[tokio::test]
async fn a_refused_first_setpoint_leaves_nothing_requested() -> Result<()> {
    // Arrange
    let hivemq = start_hivemq(&unique_network()).await?;
    let client = AsyncClient::new(format!(
        "tcp://localhost:{}",
        hivemq.get_host_port_ipv4(1883).await?
    ))?;
    client.connect(None).await?;
    let pcs = spawn(40000).await?;
    pcs.set(at::PWR_SF, (-3i16) as u16);
    let requested = Requested::default();
    // Act
    command(&client, &pcs, &requested, 5e6).await?;
    // Assert
    assert_eq!(requested_now(&requested).await, None);
    Ok(())
}
