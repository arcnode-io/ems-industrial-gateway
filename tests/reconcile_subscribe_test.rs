//! e2e: a topology change that introduces a new input topic gets it
//! subscribed. High-risk: a synthetic measurement added by a topology change
//! (e.g. a GPU node's total) otherwise never sees its inputs and stays silent
//! with no error until someone restarts the gateway.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::start_hivemq;
use futures::stream::StreamExt;
use paho_mqtt::{AsyncClient, AsyncReceiver, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SITE_ID: &str = "site_001";

/// A synthetic `sum` over one input measurement of device `sensor`.
fn total_of(input: &str) -> Value {
    json!({
        "unit": "watts", "poll_rate_hz": 2.0, "protocol": "synthetic", "operation": "sum",
        "inputs": [format!("sites/{{site_id}}/devices/sensor/measurements/{input}/watts")],
    })
}

/// Spec with a synthetic total for each named input.
fn spec(version: &str, inputs: &[&str]) -> Value {
    let totals: serde_json::Map<String, Value> = inputs
        .iter()
        .map(|i| (format!("total_{i}"), total_of(i)))
        .collect();
    json!({ "info": { "version": version }, "x-protocol-source": { "site_totals": totals } })
}

async fn serve(server: &MockServer, body: Value) {
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/asyncapi"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

/// Keep publishing `input` until `total` comes back, or give up.
async fn total_follows(
    op: &AsyncClient,
    stream: &mut AsyncReceiver<Option<Message>>,
    input: &str,
    total: &str,
) -> Result<bool> {
    let input_topic = format!("sites/{SITE_ID}/devices/sensor/measurements/{input}/watts");
    let found = timeout(Duration::from_secs(20), async {
        loop {
            op.publish(Message::new(
                input_topic.as_str(),
                r#"{"ts":"t","value":42}"#,
                0,
            ))
            .await?;
            if let Ok(Some(Some(msg))) = timeout(Duration::from_millis(500), stream.next()).await
                && msg.topic().contains(&format!("/{total}/"))
            {
                return anyhow::Ok(());
            }
        }
    })
    .await;
    Ok(matches!(found, Ok(Ok(()))))
}

#[tokio::test]
async fn an_input_added_by_a_topology_change_is_subscribed() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    // Arrange — gateway boots on a topology with one synthetic total
    let network = fixtures::containers::unique_network();
    let hivemq = start_hivemq(&network).await?;
    let broker_url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let stub = MockServer::start().await;
    serve(&stub, spec("v1", &["a"])).await;
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
    let mut op = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&broker_url)
            .client_id("reconcile-op")
            .finalize(),
    )?;
    let mut stream = op.get_stream(64);
    op.connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    op.subscribe(
        format!("sites/{SITE_ID}/devices/site_totals/measurements/#"),
        0,
    )
    .await?;
    assert!(
        total_follows(&op, &mut stream, "a", "total_a").await?,
        "boot topology's total never published"
    );

    // Act — the topology gains a second total over a new input
    serve(&stub, spec("v2", &["a", "b"])).await;
    op.publish(Message::new("system/topology_changed", "{}", 1))
        .await?;

    // Assert — the new input is subscribed, so its total publishes
    assert!(
        total_follows(&op, &mut stream, "b", "total_b").await?,
        "total over the newly added input never published"
    );

    cancel.cancel();
    gateway.await??;
    Ok(())
}
