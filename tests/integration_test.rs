//! E2E: validate the AsyncAPI/Modbus/MQTT contract end-to-end.
//! 4 testcontainers + real gateway binary in-process.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{
    start_device_api, start_emqx, start_mock_modbus_server, start_postgres,
};
use futures::StreamExt;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder};
use serde_json::Value;
use std::time::Duration;
use tokio::time::timeout;

#[tokio::test]
async fn gateway_reads_modbus_and_publishes_to_mqtt() -> Result<()> {
    // Arrange — spin up testcontainers in parallel
    let (pg, emqx, modbus_fix) =
        tokio::try_join!(start_postgres(), start_emqx(), start_mock_modbus_server(),)?;
    // pg + emqx are on the shared Docker network — device-api reaches them by hostname.
    // We hold the handles so they stay running, but don't need to read their host/port.
    let _ = (&pg, &emqx);
    let emqx_port = emqx.get_host_port_ipv4(1883).await?;
    let modbus_host = modbus_fix.get_host().await?;
    let modbus_port = modbus_fix.get_host_port_ipv4(502).await?;

    let device_api = start_device_api().await?;
    let device_api_port = device_api.get_host_port_ipv4(3000).await?;

    // Wire the fixture's host:port into the seed DTM.
    let dtm_template = include_str!("fixtures/seed_dtm.json");
    let dtm_json: Value = serde_json::from_str(dtm_template)?;
    let mut dtm = dtm_json.as_object().unwrap().clone();
    let mut devices = dtm["devices"].as_object().unwrap().clone();
    let mut meter = devices["meter_01"].as_object().unwrap().clone();
    let mut connection = meter["connection"].as_object().unwrap().clone();
    connection.insert("host".to_string(), Value::String(modbus_host.to_string()));
    connection.insert("port".to_string(), Value::Number(modbus_port.into()));
    meter.insert("connection".to_string(), Value::Object(connection));
    devices.insert("meter_01".to_string(), Value::Object(meter));
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

    // Subscribe with a test-side MQTT client to verify the gateway's publish.
    let broker_url = format!("tcp://localhost:{emqx_port}");
    let create_opts = CreateOptionsBuilder::new()
        .server_uri(&broker_url)
        .client_id("e2e-test-subscriber")
        .finalize();
    let mut sub = AsyncClient::new(create_opts)?;
    let mut stream = sub.get_stream(64);
    sub.connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    sub.subscribe(
        "sites/site_001/devices/meter_01/measurements/kwh_delivered/watt_hours",
        1,
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

    // Assert — the MQTT message arrives with kwh_delivered = 1_000_000
    let received = timeout(Duration::from_secs(5), stream.next()).await?;
    let msg = received.flatten().expect("expected MQTT message");
    let payload: Value = serde_json::from_slice(msg.payload())?;
    assert_eq!(payload["value"].as_f64().unwrap(), 1_000_000.0);

    Ok(())
}
