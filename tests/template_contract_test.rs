//! Contract: every template device-api ships becomes a spec the gateway
//! runs in full — no entry skipped, every payload schema usable.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::{http::client::fetch_asyncapi, payload};
use fixtures::catalog::{one_of_each, shipped_catalog};
use fixtures::containers::{start_device_api, start_hivemq, start_postgres, unique_network};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};

/// `device.channel` keys of a source map (raw JSON object of objects).
fn entries(map: &Value) -> BTreeSet<String> {
    map.as_object()
        .into_iter()
        .flatten()
        .flat_map(|(device, channels)| {
            channels
                .as_object()
                .into_iter()
                .flatten()
                .map(move |(ch, _)| format!("{device}.{ch}"))
        })
        .collect()
}

/// `device.channel` entries in the raw spec that the gateway didn't keep.
fn skipped<T>(raw: &Value, parsed: &HashMap<String, HashMap<String, T>>) -> Vec<String> {
    entries(raw)
        .into_iter()
        .filter(|e| {
            let (d, c) = e.split_once('.').unwrap_or_default();
            !parsed.get(d).is_some_and(|chs| chs.contains_key(c))
        })
        .collect()
}

#[tokio::test]
async fn every_shipped_template_runs_on_the_gateway() -> Result<()> {
    // Arrange
    let network = unique_network();
    let (_pg, _hivemq) = tokio::try_join!(start_postgres(&network), start_hivemq(&network))?;
    let device_api = start_device_api(&network).await?;
    let url = format!(
        "http://localhost:{}",
        device_api.get_host_port_ipv4(3000).await?
    );
    let catalog = shipped_catalog(&device_api).await?;
    let resp = reqwest::Client::new()
        .post(format!("{url}/topology"))
        .json(&one_of_each(&catalog))
        .send()
        .await?;
    let status = resp.status();
    assert_eq!(status, 201, "POST /topology: {}", resp.text().await?);

    // Act
    let raw: Value = reqwest::get(format!("{url}/asyncapi"))
        .await?
        .json()
        .await?;
    let spec = fetch_asyncapi(&url).await?;

    // Assert — the gateway kept every entry device-api emitted
    let skipped_measurements = skipped(&raw["x-protocol-source"], &spec.x_protocol_source);
    assert!(
        skipped_measurements.is_empty(),
        "measurements the gateway skipped: {skipped_measurements:?}"
    );
    let skipped_commands = skipped(&raw["x-command-source"], &spec.x_command_source);
    assert!(
        skipped_commands.is_empty(),
        "commands the gateway skipped: {skipped_commands:?}"
    );
    // ...and has a usable payload schema for every measurement
    let unusable: Vec<String> = spec
        .x_protocol_source
        .iter()
        .flat_map(|(d, chs)| chs.iter().map(move |(c, s)| (d, c, s)))
        .filter_map(|(d, c, s)| {
            let Some(reference) = &s.payload else {
                return Some(format!("{d}.{c}: no payload schema"));
            };
            payload::resolve(reference, &spec.components.schemas)
                .err()
                .map(|e| format!("{d}.{c}: {e:#}"))
        })
        .collect();
    assert!(
        unusable.is_empty(),
        "unusable payload schemas: {unusable:#?}"
    );
    assert!(!spec.x_protocol_source.is_empty());
    Ok(())
}
