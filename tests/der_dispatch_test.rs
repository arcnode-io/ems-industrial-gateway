//! e2e: der_dispatch/actual_active_power — the gateway's site-total publish
//! side of the Phase III shortfall-detection contract. Proves the gateway
//! identifies bess_module_1 as a distribute-parent (a Distribute-bound
//! set_active_power command), subscribes to its own active_power topic, and
//! republishes the sum on der_dispatch's fixed, non-templated topic — the
//! same convention ems-der-control-api already uses for target_active_power.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::start_hivemq;
use fixtures::spec_stub::spawn_asyncapi_stub;
use futures::stream::StreamExt;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde_json::{Value, json};
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
const MODULE_ID: &str = "bess_module_1";

#[tokio::test]
async fn publishes_site_total_summed_from_distribute_parent_devices() -> Result<()> {
    init_tracing();
    // Arrange — a distribute-parent device (bess_module_1) marks itself as
    // one via a Distribute-bound set_active_power command; no real racks
    // needed since der_dispatch sums the module's OWN active_power topic.
    let network = fixtures::containers::unique_network();
    let hivemq = start_hivemq(&network).await?;
    let hivemq_port = hivemq.get_host_port_ipv4(1883).await?;
    let body = json!({
        "info": { "version": "v1" },
        "x-protocol-source": {},
        "x-command-source": {
            MODULE_ID: {
                "set_active_power": {
                    "verb": "set", "target": "active_power", "unit": "watts",
                    "protocol": "distribute",
                    "allocation_policy": "equal_split",
                    "children": [],
                }
            }
        }
    });
    let stub = spawn_asyncapi_stub(body).await;

    let broker_url = format!("tcp://localhost:{hivemq_port}");
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
    let gateway_handle = {
        let cancel = cancel.clone();
        tokio::spawn(async move { app::run(cfg, cancel).await })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut operator = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&broker_url)
            .client_id("der-dispatch-test-op")
            .finalize(),
    )?;
    let mut events = operator.get_stream(64);
    operator
        .connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    operator
        .subscribe(
            format!("sites/{SITE_ID}/devices/der_dispatch/measurements/actual_active_power/watts"),
            0,
        )
        .await?;

    // Act — the module's own real (or, here, stand-in) active_power reading.
    operator
        .publish(Message::new(
            format!("sites/{SITE_ID}/devices/{MODULE_ID}/measurements/active_power/watts"),
            r#"{"ts":"t","value":275000.0}"#,
            0,
        ))
        .await?;

    // Assert — der_dispatch republishes that as the site total.
    let value = timeout(Duration::from_secs(10), async {
        loop {
            let msg = events.next().await.flatten().expect("stream closed early");
            let v: Value = serde_json::from_slice(msg.payload())?;
            let value = v["value"].as_f64().expect("non-numeric payload");
            if (value - 275_000.0).abs() < f64::EPSILON {
                return anyhow::Ok(value);
            }
        }
    })
    .await??;
    assert!((value - 275_000.0).abs() < f64::EPSILON);

    cancel.cancel();
    gateway_handle.await??;
    operator.disconnect(None).await?;
    Ok(())
}
