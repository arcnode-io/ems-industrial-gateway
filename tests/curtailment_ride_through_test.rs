//! e2e: compute rides through a curtailment on battery. When the utility
//! asks the site to shed load, the BESS covers the whole ask and the GPUs
//! keep running at full power, unthrottled. The gateway meets the
//! curtailment by dispatching the rack, never by touching the GPU node, and
//! the GPU telemetry it publishes (Redfish power as a number, NVIDIA's
//! throttle reason as our label) shows full power and no throttle throughout.

mod fixtures;

use anyhow::Result;
use axum::{Json, Router, routing::get};
use ems_industrial_gateway::modbus::client::{WordOrder, decode_int32, read_holding};
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{start_hivemq, start_mock_modbus_server_writable};
use fixtures::spec_stub::spawn_asyncapi_stub;
use futures::stream::StreamExt;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const SITE_ID: &str = "local_site";
const MODULE: &str = "module_1";
const RACK: &str = "rack_1";
const GPU_NODE: &str = "gpu_node_01";
/// The utility's curtailment ask.
const CURTAILMENT_W: i32 = 836_600;
/// One B200 at full load.
const GPU_FULL_POWER_W: f64 = 1000.0;
const GPU_BASE: &str = "/redfish/v1/Systems/HGX_Baseboard_0/Processors/GPU_SXM_1";

/// In-process BMC for one GPU at full power, unthrottled.
async fn spawn_gpu_bmc() -> Result<u16> {
    let app = Router::new()
        .route(
            &format!("{GPU_BASE}/EnvironmentMetrics"),
            get(|| async { Json(json!({ "PowerWatts": { "Reading": GPU_FULL_POWER_W } })) }),
        )
        .route(
            &format!("{GPU_BASE}/ProcessorMetrics"),
            get(|| async { Json(json!({ "Oem": { "Nvidia": { "ThrottleReasons": ["NA"] } } })) }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(async move { axum::serve(listener, app).await });
    Ok(port)
}

/// The two GPU readings' payload schemas, as device-api's AsyncAPI
/// declares them: power a number, throttle reason one of our labels.
fn gpu_schemas() -> Value {
    let sample = |value: Value| {
        json!({
            "type": "object", "required": ["ts", "value"],
            "properties": { "ts": { "type": "string", "format": "date-time" }, "value": value },
        })
    };
    json!({
        "GpuNode_Gpu1Power": sample(json!({ "type": "number" })),
        "GpuNode_Gpu1ThrottleReason": sample(json!({ "type": "string", "enum": ["NA", "SW_POWER_CAP"] })),
    })
}

/// gpu_node's per-GPU bindings, as edp-api's gpu_node.yaml declares them.
fn gpu_measurements(port: u16) -> Value {
    let redfish = |uri: &str, pointer: &str, extra: Value| {
        let mut b = json!({
            "unit": "none", "poll_rate_hz": 1.0, "protocol": "redfish",
            "host": "127.0.0.1", "port": port, "uri": uri, "json_pointer": pointer,
        });
        b.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        b
    };
    json!({
        "gpu_1_power": redfish(
            "/Systems/HGX_Baseboard_0/Processors/GPU_SXM_1/EnvironmentMetrics",
            "/PowerWatts/Reading",
            json!({ "unit": "watts", "payload": { "$ref": "#/components/schemas/GpuNode_Gpu1Power" } }),
        ),
        "gpu_1_throttle_reason": redfish(
            "/Systems/HGX_Baseboard_0/Processors/GPU_SXM_1/ProcessorMetrics",
            "/Oem/Nvidia/ThrottleReasons/0",
            json!({
                "payload": { "$ref": "#/components/schemas/GpuNode_Gpu1ThrottleReason" },
                "value_map": { "NA": "NA", "SWPowerCap": "SW_POWER_CAP" },
            }),
        ),
    })
}

fn rack_command(port: u16) -> Value {
    json!({
        "verb": "set", "target": "active_power", "unit": "watts",
        "protocol": "modbus_tcp", "host": "127.0.0.1", "port": port,
        "unit_id": "1", "address": 50, "scale": 1.0, "offset": 0.0,
    })
}

fn module_command() -> Value {
    json!({
        "verb": "set", "target": "active_power", "unit": "watts",
        "protocol": "distribute", "allocation_policy": "equal_split",
        "power_min": -4_000_000.0, "power_max": 4_000_000.0,
        "children": [{
            "device_id": RACK,
            "operating_state_topic": format!("sites/{{site_id}}/devices/{RACK}/measurements/operating_state/none"),
            "state_of_charge_topic": format!("sites/{{site_id}}/devices/{RACK}/measurements/state_of_charge/percent"),
            "power_min": -4_000_000.0, "power_max": 4_000_000.0,
        }],
    })
}

async fn publish(op: &AsyncClient, topic: &str, value: &str) -> Result<()> {
    let topic = format!("sites/{SITE_ID}/devices/{topic}");
    op.publish(Message::new(
        topic,
        format!(r#"{{"ts":"t","value":{value}}}"#),
        0,
    ))
    .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "beyond the gateway's own contracts (whole-chain or device-api internals); skipped pending pruning"]
async fn a_curtailment_is_covered_by_battery_while_gpus_stay_at_full_power() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    // Arrange — one module over one rack, one GPU node at full power
    let network = fixtures::containers::unique_network();
    let hivemq = start_hivemq(&network).await?;
    let broker_url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let rack = start_mock_modbus_server_writable().await?;
    let rack_port = rack.get_host_port_ipv4(502).await?;
    let bmc_port = spawn_gpu_bmc().await?;
    let stub = spawn_asyncapi_stub(json!({
        "info": { "version": "v1" },
        "x-protocol-source": { GPU_NODE: gpu_measurements(bmc_port) },
        "components": { "schemas": gpu_schemas() },
        "x-command-source": {
            MODULE: { "set_active_power": module_command() },
            RACK: { "set_active_power": rack_command(rack_port) },
        }
    }))
    .await;
    unsafe {
        std::env::set_var("MQTT_GATEWAY_PASSWORD", "test");
    }
    let cfg = Config {
        device_api_url: stub.uri(),
        broker_url: broker_url.clone(),
        mqtt_username: "arcnode_gateway".to_string(),
        site_id: SITE_ID.to_string(),
        log_level: "info".to_string(),
        gateway_credentials: None,
    };
    let cancel = CancellationToken::new();
    let gateway = {
        let cancel = cancel.clone();
        tokio::spawn(async move { app::run(cfg, cancel).await })
    };
    fixtures::readiness::wait_for_gateway_ready(&broker_url, SITE_ID, &[MODULE]).await?;
    let mut op = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&broker_url)
            .client_id("ride-through-op")
            .finalize(),
    )?;
    let mut gpu_telemetry = op.get_stream(64);
    op.connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    op.subscribe(
        format!("sites/{SITE_ID}/devices/{GPU_NODE}/measurements/#"),
        0,
    )
    .await?;
    publish(
        &op,
        &format!("{RACK}/measurements/operating_state/none"),
        "0",
    )
    .await?;
    publish(
        &op,
        &format!("{RACK}/measurements/state_of_charge/percent"),
        "60",
    )
    .await?;
    publish(
        &op,
        &format!("{MODULE}/measurements/state_of_charge/percent"),
        "60",
    )
    .await?;

    // Act — the utility's curtailment arrives
    publish(
        &op,
        "der_dispatch/measurements/target_active_power/watts",
        &CURTAILMENT_W.to_string(),
    )
    .await?;
    publish(&op, "der_dispatch/measurements/event_active/none", "true").await?;

    // Assert — the rack carries the whole ask...
    timeout(Duration::from_secs(20), async {
        loop {
            // A read error means the rack's setpoint register isn't written
            // yet (the mock serves it only after the first write): keep waiting.
            if let Ok(words) = read_holding("127.0.0.1", rack_port, 1, 50, 2).await
                && decode_int32(&words, WordOrder::HighLow) == CURTAILMENT_W
            {
                return anyhow::Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("rack never reached the {CURTAILMENT_W} W curtailment"))??;

    // ...while the GPU keeps full power and reports no throttle
    let (mut power, mut throttle) = (None, None);
    timeout(Duration::from_secs(15), async {
        while power.is_none() || throttle.is_none() {
            let Some(Some(msg)) = gpu_telemetry.next().await else {
                continue;
            };
            let value = Some(serde_json::from_slice::<Value>(msg.payload())?["value"].clone());
            if msg.topic().contains("/gpu_1_power/") {
                power = value;
            } else if msg.topic().contains("/gpu_1_throttle_reason/") {
                throttle = value;
            }
        }
        anyhow::Ok(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("no GPU telemetry during the curtailment"))??;
    assert_eq!(
        power,
        Some(json!(GPU_FULL_POWER_W)),
        "GPU power during the curtailment"
    );
    assert_eq!(
        throttle,
        Some(json!("NA")),
        "GPU throttle reason during the curtailment"
    );

    cancel.cancel();
    gateway.await??;
    Ok(())
}
