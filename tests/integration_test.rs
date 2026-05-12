//! E2E: validate the AsyncAPI / Modbus / SNMP / MQTT contract end-to-end.
//! 5 testcontainers + real gateway binary in-process.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{
    start_device_api, start_emqx, start_mock_modbus_server, start_mock_redfish_service,
    start_mock_snmp_agent, start_postgres,
};
use futures::StreamExt;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use testcontainers::core::ContainerPort;
use tokio::time::timeout;

#[tokio::test]
async fn gateway_reads_three_protocols_and_publishes_to_mqtt() -> Result<()> {
    // Arrange — spin up testcontainers in parallel
    let (pg, emqx, modbus_fix, snmp_fix, redfish_fix) = tokio::try_join!(
        start_postgres(),
        start_emqx(),
        start_mock_modbus_server(),
        start_mock_snmp_agent(),
        start_mock_redfish_service(),
    )?;
    let _ = (&pg, &emqx);
    let emqx_port = emqx.get_host_port_ipv4(1883).await?;
    let modbus_host = modbus_fix.get_host().await?;
    let modbus_port = modbus_fix.get_host_port_ipv4(502).await?;
    let snmp_host = snmp_fix.get_host().await?;
    let snmp_port = snmp_fix.get_host_port_ipv4(ContainerPort::Udp(161)).await?;
    let redfish_host = redfish_fix.get_host().await?;
    let redfish_port = redfish_fix.get_host_port_ipv4(8443).await?;

    let device_api = start_device_api().await?;
    let device_api_port = device_api.get_host_port_ipv4(3000).await?;

    // Wire fixture host:port into seed DTM (meter_01 → modbus; pdu_01 → snmp; switch_01 → redfish).
    let dtm_template = include_str!("fixtures/seed_dtm.json");
    let dtm_json: Value = serde_json::from_str(dtm_template)?;
    let mut dtm = dtm_json.as_object().unwrap().clone();
    let mut devices = dtm["devices"].as_object().unwrap().clone();

    for (device_id, host, port) in [
        ("meter_01", modbus_host.to_string(), modbus_port),
        ("pdu_01", snmp_host.to_string(), snmp_port),
        ("switch_01", redfish_host.to_string(), redfish_port),
    ] {
        let mut device = devices[device_id].as_object().unwrap().clone();
        let mut connection = device["connection"].as_object().unwrap().clone();
        connection.insert("host".to_string(), Value::String(host));
        connection.insert("port".to_string(), Value::Number(port.into()));
        device.insert("connection".to_string(), Value::Object(connection));
        devices.insert(device_id.to_string(), Value::Object(device));
    }
    dtm.insert("devices".to_string(), Value::Object(devices));
    let dtm_body = Value::Object(dtm);

    let device_api_url = format!("http://localhost:{device_api_port}");
    let post_resp = reqwest::Client::new()
        .post(format!("{device_api_url}/topology"))
        .json(&dtm_body)
        .send()
        .await?;
    let status = post_resp.status();
    if status != 201 {
        let body = post_resp.text().await?;
        panic!("POST /topology failed: status={status} body={body}");
    }

    // Subscribe to both expected topics.
    let broker_url = format!("tcp://localhost:{emqx_port}");
    let create_opts = CreateOptionsBuilder::new()
        .server_uri(&broker_url)
        .client_id("e2e-test-subscriber")
        .finalize();
    let mut sub = AsyncClient::new(create_opts)?;
    let mut stream = sub.get_stream(64);
    sub.connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    sub.subscribe_many(
        &[
            "sites/site_001/devices/meter_01/measurements/kwh_delivered/watt_hours".to_string(),
            "sites/site_001/devices/pdu_01/measurements/input_current/amps".to_string(),
            "sites/site_001/devices/switch_01/measurements/inlet_temp/celsius".to_string(),
        ],
        &[1, 1, 1],
    )
    .await?;

    // Act — run the gateway one-shot.
    let cfg = Config {
        device_api_url,
        broker_url: broker_url.clone(),
        site_id: "site_001".to_string(),
        log_level: "info".to_string(),
    };
    app::run(cfg).await?;

    // Assert — collect all three publishes within 10s, verify each value's range.
    let mut received: HashMap<String, f64> = HashMap::new();
    while received.len() < 3 {
        let msg = timeout(Duration::from_secs(10), stream.next())
            .await?
            .flatten()
            .expect("expected MQTT message");
        let payload: Value = serde_json::from_slice(msg.payload())?;
        received.insert(msg.topic().to_string(), payload["value"].as_f64().unwrap());
    }

    let kwh = received
        .get("sites/site_001/devices/meter_01/measurements/kwh_delivered/watt_hours")
        .expect("modbus publish missing");
    assert!(
        (1_000_000.0..=1_010_000.0).contains(kwh),
        "kwh_delivered {kwh} outside expected sawtooth range [1_000_000, 1_010_000]",
    );

    let amps = received
        .get("sites/site_001/devices/pdu_01/measurements/input_current/amps")
        .expect("snmp publish missing");
    assert!(
        (100.0..=200.0).contains(amps),
        "input_current {amps} outside expected sawtooth range [100, 200]",
    );

    let inlet = received
        .get("sites/site_001/devices/switch_01/measurements/inlet_temp/celsius")
        .expect("redfish publish missing");
    assert!(
        (20.0..=30.0).contains(inlet),
        "inlet_temp {inlet} outside expected sawtooth range [20, 30]",
    );

    Ok(())
}
