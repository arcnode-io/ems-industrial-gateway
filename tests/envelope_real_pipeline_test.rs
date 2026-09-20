//! e2e: full real pipeline — real edp-api templates (bess_rack, bess_module),
//! ingested by a real device-api container via POST /topology, resolved into
//! a real AsyncAPI spec, consumed by the gateway to make real Modbus writes.
//!
//! Distinct from distribute_test.rs/envelope_test.rs, which hand-roll an
//! AsyncAPI stub to isolate the allocation/control-law wiring. This test
//! proves the templates themselves (tests/fixtures/bess_templates.json,
//! transcribed from edp-api's real device_templates/{leaf/bess_rack,
//! module/bess_module}.yaml as of edp-api@0fe1856) pass device-api's Zod
//! validation and resolve to the same shape those focused tests assume —
//! the "real device-api-container pass" per handoff-envelope-control-law.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::modbus::client::{WordOrder, decode_int32, read_holding};
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{
    start_device_api, start_hivemq, start_mock_modbus_server_writable, start_postgres,
};
use fixtures::real_dtm::{MODULE_ID, bess_dtm, seed_rack};
use futures::stream::StreamExt;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde_json::Value;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

static TRACING_INIT: OnceLock<()> = OnceLock::new();
fn init_tracing() {
    TRACING_INIT.get_or_init(|| {
        let _ = tracing_subscriber::fmt::try_init();
    });
}

const SITE_ID: &str = "site_001";
/// bess_rack.yaml: commands.set_active_power.binding.address.
const CMD_REGISTER: u16 = 50;

async fn read_cmd_register(port: u16) -> Result<i32> {
    let words = read_holding("127.0.0.1", port, 1, CMD_REGISTER, 2).await?;
    Ok(decode_int32(&words, WordOrder::HighLow))
}

#[tokio::test]
async fn real_templates_resolve_and_distribute_soc_weighted_write() -> Result<()> {
    init_tracing();
    // Arrange — full real stack: postgres + hivemq + device-api on their own
    // per-test network, two writable mock-modbus racks reachable from the host.
    let network = fixtures::containers::unique_network();
    let (pg, hivemq) = tokio::try_join!(start_postgres(&network), start_hivemq(&network))?;
    let _ = &pg;
    let hivemq_port = hivemq.get_host_port_ipv4(1883).await?;
    let device_api = start_device_api(&network).await?;
    let device_api_port = device_api.get_host_port_ipv4(3000).await?;
    let rack1 = start_mock_modbus_server_writable().await?;
    let rack1_modbus = rack1.get_host_port_ipv4(502).await?;
    let rack1_control = rack1.get_host_port_ipv4(8080).await?;
    let rack2 = start_mock_modbus_server_writable().await?;
    let rack2_modbus = rack2.get_host_port_ipv4(502).await?;
    let rack2_control = rack2.get_host_port_ipv4(8080).await?;

    // rack_1 at 70% SoC, rack_2 at 30% -> soc_weighted split should be 70/30.
    seed_rack(rack1_control, 700).await?;
    seed_rack(rack2_control, 300).await?;

    let dtm = bess_dtm(rack1_modbus, rack2_modbus);
    let device_api_url = format!("http://localhost:{device_api_port}");
    let resp = reqwest::Client::new()
        .post(format!("{device_api_url}/topology"))
        .json(&dtm)
        .send()
        .await?;
    let status = resp.status();
    if status != 201 {
        let body = resp.text().await?;
        panic!("POST /topology failed: status={status} body={body}");
    }

    let broker_url = format!("tcp://localhost:{hivemq_port}");
    unsafe {
        std::env::set_var("MQTT_GATEWAY_PASSWORD", "test");
    }
    let cfg = Config {
        device_api_url,
        broker_url: broker_url.clone(),
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
    // Real Modbus pollers need a beat longer than the stub tests: with real
    // x-protocol-source bindings, dispatch's soc_weighted allocation needs
    // rack_1/rack_2's real state_of_charge/operating_state polls (1 Hz) to
    // have actually landed in cache before a command arrives, not just the
    // AsyncAPI fetch to complete.
    tokio::time::sleep(Duration::from_secs(6)).await;
    if gateway_handle.is_finished() {
        let res = gateway_handle.await;
        panic!("gateway task exited early: {res:?}");
    }

    let mut operator = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&broker_url)
            .client_id("real-pipeline-op")
            .finalize(),
    )?;
    let mut events = operator.get_stream(64);
    operator
        .connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    operator
        .subscribe(
            format!("sites/{SITE_ID}/devices/{MODULE_ID}/events/dispatch_state"),
            1,
        )
        .await?;

    // Generous envelope so headroom never binds — proves the direct dispatch
    // path, not the autonomous clamp (already covered by envelope_test.rs).
    for (topic, value) in [
        (
            "operating_envelope/measurements/import_limit/watts",
            10_000_000.0,
        ),
        (
            "operating_envelope/measurements/export_limit/watts",
            10_000_000.0,
        ),
    ] {
        operator
            .publish(Message::new(
                format!("sites/{SITE_ID}/devices/{topic}"),
                format!(r#"{{"ts":"t","value":{value}}}"#),
                0,
            ))
            .await?;
    }

    // Act — one real operator command: 400kW, soc_weighted(70/30) -> 280k/120k.
    operator
        .publish(Message::new(
            format!("sites/{SITE_ID}/devices/{MODULE_ID}/commands/set/active_power/watts"),
            r#"{"ts":"t","value":400000,"command_id":"cmd-real-1"}"#,
            1,
        ))
        .await?;

    let mut phases = Vec::new();
    while phases.len() < 2 {
        let msg = timeout(Duration::from_secs(15), events.next())
            .await?
            .flatten()
            .expect("dispatch_state stream closed early");
        let v: Value = serde_json::from_slice(msg.payload())?;
        phases.push(v["phase"].as_str().unwrap_or_default().to_string());
    }
    assert_eq!(phases, vec!["received", "done"]);

    // Assert — real Modbus writes landed with the real SoC-weighted split.
    assert_eq!(read_cmd_register(rack1_modbus).await?, 280_000);
    assert_eq!(read_cmd_register(rack2_modbus).await?, 120_000);

    cancel.cancel();
    gateway_handle.await??;
    operator.disconnect(None).await?;
    Ok(())
}
