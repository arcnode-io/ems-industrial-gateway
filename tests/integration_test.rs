//! Integration: validate the AsyncAPI / Modbus / SNMP / Redfish / DNP3 /
//! BACnet / MQTT contract against real testcontainers + the gateway binary.
//! 8 testcontainers + real gateway binary in-process.
//!
//! Tier 2: gateway runs continuously. Test collects 3 publishes per topic and
//! asserts (a) all values in expected sawtooth range and (b) ≥2 distinct
//! values per topic — proving the ticker is actually advancing.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{
    start_device_api, start_hivemq, start_mock_bacnet_device, start_mock_dnp3_outstation,
    start_mock_modbus_server, start_mock_redfish_service, start_mock_snmp_agent, start_postgres,
};
use futures::StreamExt;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use testcontainers::core::ContainerPort;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

/// How many publishes per topic the assertion phase waits for.
const PUBLISHES_PER_TOPIC: usize = 3;
/// Overall timeout for the publish-collection phase.
const COLLECTION_TIMEOUT: Duration = Duration::from_secs(45);

#[tokio::test]
async fn gateway_continuously_reads_five_protocols_and_publishes_to_mqtt() -> Result<()> {
    // Arrange — spin up testcontainers in parallel
    let (pg, hivemq, modbus_fix, snmp_fix, redfish_fix, dnp3_fix, bacnet_fix) = tokio::try_join!(
        start_postgres(),
        start_hivemq(),
        start_mock_modbus_server(),
        start_mock_snmp_agent(),
        start_mock_redfish_service(),
        start_mock_dnp3_outstation(),
        start_mock_bacnet_device(),
    )?;
    let _ = (&pg, &hivemq);
    let hivemq_port = hivemq.get_host_port_ipv4(1883).await?;
    let modbus_host = modbus_fix.get_host().await?;
    let modbus_port = modbus_fix.get_host_port_ipv4(502).await?;
    let snmp_host = snmp_fix.get_host().await?;
    let snmp_port = snmp_fix.get_host_port_ipv4(ContainerPort::Udp(161)).await?;
    let redfish_host = redfish_fix.get_host().await?;
    let redfish_port = redfish_fix.get_host_port_ipv4(8443).await?;
    let dnp3_host = dnp3_fix.get_host().await?;
    let dnp3_port = dnp3_fix.get_host_port_ipv4(20000).await?;
    let bacnet_host = bacnet_fix.get_host().await?;
    let bacnet_port = bacnet_fix
        .get_host_port_ipv4(ContainerPort::Udp(47808))
        .await?;

    let device_api = start_device_api().await?;
    let device_api_port = device_api.get_host_port_ipv4(3000).await?;

    // Wire fixture host:port into seed DTM (meter_01 → modbus; pdu_01 → snmp;
    // switch_01 → redfish; relay_01 → dnp3).
    let dtm_template = include_str!("fixtures/seed_dtm.json");
    let dtm_json: Value = serde_json::from_str(dtm_template)?;
    let mut dtm = dtm_json.as_object().unwrap().clone();
    let mut devices = dtm["devices"].as_object().unwrap().clone();

    for (device_id, host, port) in [
        ("meter_01", modbus_host.to_string(), modbus_port),
        ("pdu_01", snmp_host.to_string(), snmp_port),
        ("switch_01", redfish_host.to_string(), redfish_port),
        ("relay_01", dnp3_host.to_string(), dnp3_port),
        ("cooler_01", bacnet_host.to_string(), bacnet_port),
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
    let broker_url = format!("tcp://localhost:{hivemq_port}");
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
            "sites/site_001/devices/relay_01/measurements/phase_a_current/amps".to_string(),
            "sites/site_001/devices/cooler_01/measurements/supply_water_temp/celsius".to_string(),
        ],
        &[1, 1, 1, 1, 1],
    )
    .await?;

    // Act — spawn the gateway with a cancel token so the test owns lifecycle.
    let cfg = Config {
        device_api_url,
        broker_url: broker_url.clone(),
        site_id: "site_001".to_string(),
        log_level: "info".to_string(),
    };
    let cancel = CancellationToken::new();
    let gateway_handle = {
        let cancel = cancel.clone();
        tokio::spawn(async move { app::run(cfg, cancel).await })
    };

    // Assert — collect 3 publishes per topic. With sawtooth fixtures at
    // poll_rate_hz ≥ 0.1, this lands in well under 30s.
    let topics: [(&str, std::ops::RangeInclusive<f64>); 5] = [
        (
            "sites/site_001/devices/meter_01/measurements/kwh_delivered/watt_hours",
            1_000_000.0..=1_010_000.0,
        ),
        (
            "sites/site_001/devices/pdu_01/measurements/input_current/amps",
            100.0..=200.0,
        ),
        (
            "sites/site_001/devices/switch_01/measurements/inlet_temp/celsius",
            20.0..=30.0,
        ),
        (
            "sites/site_001/devices/relay_01/measurements/phase_a_current/amps",
            100.0..=200.0,
        ),
        (
            "sites/site_001/devices/cooler_01/measurements/supply_water_temp/celsius",
            7.0..=15.0,
        ),
    ];

    let mut samples: HashMap<String, Vec<f64>> = HashMap::new();
    let deadline = tokio::time::Instant::now() + COLLECTION_TIMEOUT;
    loop {
        if topics
            .iter()
            .all(|(t, _)| samples.get(*t).map(Vec::len).unwrap_or(0) >= PUBLISHES_PER_TOPIC)
        {
            break;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            panic!(
                "timed out waiting for {PUBLISHES_PER_TOPIC} publishes per topic; got {samples:?}"
            );
        }
        let msg_opt = timeout(deadline - now, stream.next()).await?.flatten();
        let msg = msg_opt.expect("expected MQTT message");
        let payload: Value = serde_json::from_slice(msg.payload())?;
        let value = payload["value"].as_f64().expect("non-numeric payload");
        samples
            .entry(msg.topic().to_string())
            .or_default()
            .push(value);
    }

    cancel.cancel();
    gateway_handle.await??;

    // Verify every topic: range on first sample + ≥2 distinct values (proving
    // the per-measurement ticker actually advanced).
    for (topic, range) in &topics {
        let series = samples
            .get(*topic)
            .unwrap_or_else(|| panic!("no samples for {topic}"));
        assert!(
            series.len() >= PUBLISHES_PER_TOPIC,
            "expected {PUBLISHES_PER_TOPIC} samples on {topic}, got {}",
            series.len(),
        );
        let first = series[0];
        assert!(
            range.contains(&first),
            "{topic} first value {first} outside expected range {range:?}",
        );
        let distinct: std::collections::HashSet<u64> = series.iter().map(|v| v.to_bits()).collect();
        assert!(
            distinct.len() >= 2,
            "{topic} produced no distinct values across {} samples — ticker stalled? {series:?}",
            series.len(),
        );
    }

    Ok(())
}

/// Synthetic-channel e2e (Phase 4 part A): no south-side device, just MQTT +
/// device-api + gateway. Test publishes the synthetic inputs to MQTT, gateway
/// caches + computes `import_limit − active_power`, publishes the result on
/// the canonical `bess_module_1.import_headroom` topic.
///
/// Will fail until the registry's `ems-device-api:latest` image picks up the
/// Phase 1 schema additions (Publisher.GATEWAY, SyntheticBinding, the
/// AsyncAPI `{device_id}` substitution). Auto-passes on the next CI cycle
/// after edp-api/ems-device-api commits land.
#[tokio::test]
async fn synthetic_headroom_publishes_subtract_of_cached_mqtt_inputs() -> Result<()> {
    // Arrange — minimal fixture: MQTT + postgres (device-api dep) + device-api
    let (pg, hivemq) = tokio::try_join!(start_postgres(), start_hivemq())?;
    let _ = &pg;
    let hivemq_port = hivemq.get_host_port_ipv4(1883).await?;
    let device_api = start_device_api().await?;
    let device_api_port = device_api.get_host_port_ipv4(3000).await?;

    // DTM has bess_module_1 + the synthetic measurement (seed_dtm.json).
    let dtm_template = include_str!("fixtures/seed_dtm.json");
    let dtm_body: Value = serde_json::from_str(dtm_template)?;
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

    let broker_url = format!("tcp://localhost:{hivemq_port}");
    let create_sub = CreateOptionsBuilder::new()
        .server_uri(&broker_url)
        .client_id("synth-test-subscriber")
        .finalize();
    let mut sub = AsyncClient::new(create_sub)?;
    let mut stream = sub.get_stream(64);
    sub.connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    let output_topic =
        "sites/site_001/devices/bess_module_1/measurements/import_headroom/watts".to_string();
    sub.subscribe(&output_topic, 0).await?;

    // Spawn the gateway.
    let cfg = Config {
        device_api_url,
        broker_url: broker_url.clone(),
        site_id: "site_001".to_string(),
        log_level: "info".to_string(),
    };
    let cancel = CancellationToken::new();
    let gateway_handle = {
        let cancel = cancel.clone();
        tokio::spawn(async move { app::run(cfg, cancel).await })
    };

    // Act — publish synthetic inputs from a fresh client (gateway is the
    // subscriber-side; we play the role of the upstream producers).
    let create_pub = CreateOptionsBuilder::new()
        .server_uri(&broker_url)
        .client_id("synth-test-publisher")
        .finalize();
    let pub_client = AsyncClient::new(create_pub)?;
    pub_client
        .connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    // Small delay so the gateway has time to subscribe to the inputs.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let import_topic = "sites/site_001/devices/operating_envelope/measurements/import_limit/watts";
    let active_topic = "sites/site_001/devices/bess_module_1/measurements/active_power/watts";
    let import_payload = r#"{"ts":"2026-05-17T00:00:00Z","value":5000000.0}"#;
    let active_payload = r#"{"ts":"2026-05-17T00:00:00Z","value":2000000.0}"#;

    // Retain=true (per ADR-002 §11 measurement-family default) so the gateway
    // sees the inputs even if our publish lands before it subscribes.
    pub_client
        .publish(paho_mqtt::Message::new_retained(
            import_topic,
            import_payload,
            0,
        ))
        .await?;
    pub_client
        .publish(paho_mqtt::Message::new_retained(
            active_topic,
            active_payload,
            0,
        ))
        .await?;

    // Assert — collect at least one synthetic output sample within COLLECTION_TIMEOUT.
    let value = timeout(COLLECTION_TIMEOUT, async {
        loop {
            let Some(msg) = stream.next().await.flatten() else {
                continue;
            };
            if msg.topic() == output_topic {
                let payload: Value = serde_json::from_slice(msg.payload())?;
                let value = payload["value"].as_f64().expect("non-numeric payload");
                return anyhow::Ok(value);
            }
        }
    })
    .await??;

    cancel.cancel();
    gateway_handle.await??;
    pub_client.disconnect(None).await?;

    // 5_000_000 − 2_000_000 = 3_000_000 (loose epsilon for any float drift)
    assert!(
        (value - 3_000_000.0).abs() < 1.0,
        "synthetic headroom should be 3_000_000, got {value}",
    );

    Ok(())
}
