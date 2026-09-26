//! e2e: BESS reserve floor through the real pipeline. A DTM with
//! `bess_reserve_floor_mwh` goes into a real device-api, which resolves
//! `state_of_charge_floor_percent` onto the module's distribute binding; the
//! gateway then withholds discharge from a rack at the floor.
//!
//! The unit tests in dispatch::distribute prove the allocation math; this one
//! proves the field actually survives the spec pipeline into the gateway.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::modbus::client::{WordOrder, decode_int32, read_holding};
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{
    start_device_api, start_hivemq, start_mock_modbus_server_writable, start_postgres,
};
use fixtures::real_dtm::{MODULE_ID, RACK_1, RACK_2, bess_dtm, seed_rack};
use futures::stream::StreamExt;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const SITE_ID: &str = "site_001";
/// bess_rack.yaml: commands.set_active_power.binding.address.
const CMD_REGISTER: u16 = 50;
/// 2 MWh reserve over two 4000 kWh racks = 25%.
const RESERVE_FLOOR_MWH: f64 = 2.0;
const EXPECTED_FLOOR_PERCENT: f64 = 25.0;

async fn read_cmd_register(port: u16) -> Result<i32> {
    let words = read_holding("127.0.0.1", port, 1, CMD_REGISTER, 2).await?;
    Ok(decode_int32(&words, WordOrder::HighLow))
}

#[tokio::test]
async fn discharge_withheld_from_rack_at_reserve_floor() -> Result<()> {
    // Arrange — real stack; rack_1 below the 25% floor, rack_2 above it.
    let network = fixtures::containers::unique_network();
    let (pg, hivemq) = tokio::try_join!(start_postgres(&network), start_hivemq(&network))?;
    let _ = &pg;
    let hivemq_port = hivemq.get_host_port_ipv4(1883).await?;
    let device_api = start_device_api(&network).await?;
    let device_api_url = format!(
        "http://localhost:{}",
        device_api.get_host_port_ipv4(3000).await?
    );
    let rack1 = start_mock_modbus_server_writable().await?;
    let rack1_modbus = rack1.get_host_port_ipv4(502).await?;
    let rack1_control = rack1.get_host_port_ipv4(8080).await?;
    let rack2 = start_mock_modbus_server_writable().await?;
    let rack2_modbus = rack2.get_host_port_ipv4(502).await?;
    seed_rack(rack1_control, 200).await?; // 20.0%
    seed_rack(rack2.get_host_port_ipv4(8080).await?, 600).await?; // 60.0%
    // Preload rack_1's command register so a write of 0 is observable.
    reqwest::Client::new()
        .put(format!("http://127.0.0.1:{rack1_control}/registers"))
        .json(&json!({ "registers": { "50": 0, "51": 12345 } }))
        .send()
        .await?
        .error_for_status()?;

    let mut dtm = bess_dtm(rack1_modbus, rack2_modbus);
    dtm["sizing_params"]["bess_reserve_floor_mwh"] = json!(RESERVE_FLOOR_MWH);
    reqwest::Client::new()
        .post(format!("{device_api_url}/topology"))
        .json(&dtm)
        .send()
        .await?
        .error_for_status()?;

    // Assert — the floor made it into the real spec the gateway will read.
    let spec: Value = reqwest::get(format!("{device_api_url}/asyncapi"))
        .await?
        .json()
        .await?;
    let floor =
        spec["x-command-source"][MODULE_ID]["set_active_power"]["state_of_charge_floor_percent"]
            .as_f64()
            .expect("state_of_charge_floor_percent missing from the resolved spec");
    assert!((floor - EXPECTED_FLOOR_PERCENT).abs() < 1e-9);

    let broker_url = format!("tcp://localhost:{hivemq_port}");
    let mut operator = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&broker_url)
            .client_id("reserve-floor-op")
            .finalize(),
    )?;
    let mut events = operator.get_stream(64);
    operator
        .connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    let rack_topics: Vec<String> = [RACK_1, RACK_2]
        .iter()
        .flat_map(|r| {
            [
                format!("sites/{SITE_ID}/devices/{r}/measurements/state_of_charge/percent"),
                format!("sites/{SITE_ID}/devices/{r}/measurements/operating_state/none"),
            ]
        })
        .collect();
    for t in &rack_topics {
        operator.subscribe(t, 0).await?;
    }
    operator
        .subscribe(
            format!("sites/{SITE_ID}/devices/{MODULE_ID}/events/dispatch_state"),
            1,
        )
        .await?;

    unsafe {
        std::env::set_var("MQTT_GATEWAY_PASSWORD", "test");
    }
    let cfg = Config {
        device_api_url,
        broker_url,
        mqtt_username: "arcnode_gateway".to_string(),
        site_id: SITE_ID.to_string(),
        log_level: "info".to_string(),
        gateway_credentials: None,
    };
    let cancel = CancellationToken::new();
    let gateway_handle = {
        let cancel = cancel.clone();
        tokio::spawn(async move { app::run(cfg, cancel).await })
    };

    // Ready once the gateway has polled and published every rack reading the
    // allocation needs — it subscribes before polling, so it has them cached too.
    let mut seen: HashSet<String> = HashSet::new();
    timeout(Duration::from_secs(60), async {
        while seen.len() < rack_topics.len() {
            let msg = events.next().await.flatten().expect("stream closed early");
            if rack_topics.contains(&msg.topic().to_string()) {
                seen.insert(msg.topic().to_string());
            }
        }
    })
    .await?;

    // Act — 200kW discharge request to the module.
    operator
        .publish(Message::new(
            format!("sites/{SITE_ID}/devices/{MODULE_ID}/commands/set/active_power/watts"),
            r#"{"ts":"t","value":200000,"command_id":"cmd-floor-1"}"#,
            1,
        ))
        .await?;
    let phase = timeout(Duration::from_secs(15), async {
        loop {
            let msg = events.next().await.flatten().expect("stream closed early");
            if msg.topic().ends_with("events/dispatch_state") {
                let v: Value = serde_json::from_slice(msg.payload()).unwrap();
                let phase = v["phase"].as_str().unwrap_or_default().to_string();
                if phase != "received" {
                    return phase;
                }
            }
        }
    })
    .await?;
    assert_eq!(phase, "done");

    // Assert — rack_1 holds its reserve (written 0), rack_2 covers it all.
    assert_eq!(read_cmd_register(rack1_modbus).await?, 0);
    assert_eq!(read_cmd_register(rack2_modbus).await?, 200_000);

    cancel.cancel();
    gateway_handle.await??;
    operator.disconnect(None).await?;
    Ok(())
}
