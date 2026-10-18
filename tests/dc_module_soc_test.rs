//! e2e: bess_module_dc's PCS children read the module's own SoC. With
//! `state_of_charge_source: module`, device-api points every pcs child's
//! `state_of_charge_topic` at bess_module_dc's state_of_charge, a
//! weighted_mean the gateway itself publishes over bess_catl. That value has
//! to come back through the broker into the gateway's input cache, or every
//! child reads as missing SoC and the DC module holds.

mod fixtures;

use anyhow::{Result, bail};
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{start_hivemq, unique_network};
use fixtures::spec_stub::spawn_lockable_stub;
use fixtures::sunspec_pcs::{Pcs, at, spawn};
use futures::StreamExt;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const SITE: &str = "local_site";
const MODULE: &str = "bess_module_dc";
const BANK: &str = "bess_catl_1";

fn topic(device: &str, rest: &str) -> String {
    format!("sites/{{site_id}}/devices/{device}/measurements/{rest}")
}

fn pcs_command(pcs: &Pcs) -> Value {
    json!({
        "verb": "set", "target": "active_power", "unit": "watts",
        "protocol": "sunspec", "host": pcs.addr.ip().to_string(), "port": pcs.addr.port(),
        "unit_id": "1", "model_id": 65534, "point": "CmdRealPwr",
    })
}

/// A child as device-api resolves it under `state_of_charge_source: module`.
fn child(device_id: &str) -> Value {
    json!({
        "device_id": device_id,
        "operating_state_topic": topic(device_id, "operating_state/none"),
        "state_of_charge_topic": topic(MODULE, "state_of_charge/percent"),
        "power_min": -1_043_000.0, "power_max": 1_043_000.0,
    })
}

fn spec(pcs_1: &Pcs, pcs_2: &Pcs) -> Value {
    json!({
        "info": { "version": "v1" },
        "x-protocol-source": { MODULE: { "state_of_charge": {
            "unit": "percent", "poll_rate_hz": 1.0, "protocol": "synthetic",
            "operation": "weighted_mean",
            "pairs": [{ "topic": topic(BANK, "state_of_charge/percent"), "weight": 1.0 }],
        }}},
        "x-command-source": {
            MODULE: { "set_active_power": {
                "verb": "set", "target": "active_power", "unit": "watts",
                "protocol": "distribute", "allocation_policy": "equal_split",
                "children": [child("pcs_1"), child("pcs_2")],
            }},
            "pcs_1": { "set_active_power": pcs_command(pcs_1) },
            "pcs_2": { "set_active_power": pcs_command(pcs_2) },
        },
    })
}

#[tokio::test]
async fn the_dc_module_dispatches_on_the_soc_the_gateway_publishes() -> Result<()> {
    // Arrange
    let hivemq = start_hivemq(&unique_network()).await?;
    let broker_url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let (pcs_1, pcs_2) = (spawn(40000).await?, spawn(40000).await?);
    let (stub, _locked) = spawn_lockable_stub(spec(&pcs_1, &pcs_2)).await;
    unsafe {
        std::env::set_var("MQTT_GATEWAY_PASSWORD", "test");
    }
    let cfg = Config {
        device_api_url: stub.uri(),
        broker_url: broker_url.clone(),
        mqtt_username: "arcnode_gateway".to_string(),
        site_id: SITE.to_string(),
        log_level: "info".to_string(),
        gateway_credentials: None,
    };
    let cancel = CancellationToken::new();
    let gateway = tokio::spawn(app::run(cfg, cancel.clone()));
    let mut op = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(&broker_url)
            .client_id("dc-module-soc-op")
            .finalize(),
    )?;
    let mut acks = op.get_stream(256);
    op.connect(ConnectOptionsBuilder::new().clean_session(true).finalize())
        .await?;
    op.subscribe(
        format!("sites/{SITE}/devices/{MODULE}/events/dispatch_state"),
        1,
    )
    .await?;

    // Act — the bank reports its SoC, the PCS units are in standby (only the
    // bank's SoC comes from outside; the module's is the gateway's own), then
    // the module is told to discharge 200 kW, re-sent until it lands
    let measure = |device: &str, rest: &str, value: f64| {
        Message::new(
            topic(device, rest).replace("{site_id}", SITE),
            json!({ "ts": "t", "value": value }).to_string(),
            1,
        )
    };
    let command = format!("sites/{SITE}/devices/{MODULE}/commands/set/active_power/watts");
    let done = timeout(Duration::from_secs(90), async {
        for n in 0.. {
            op.publish(measure(BANK, "state_of_charge/percent", 60.0))
                .await?;
            for pcs in ["pcs_1", "pcs_2"] {
                op.publish(measure(pcs, "operating_state/none", 0.0))
                    .await?;
            }
            let id = format!("c{n}");
            let frame = json!({ "ts": "t", "value": 200_000.0, "command_id": id }).to_string();
            op.publish(Message::new(command.as_str(), frame, 1)).await?;
            let phase = timeout(Duration::from_secs(3), async {
                while let Some(Some(msg)) = acks.next().await {
                    let ack: Value = serde_json::from_slice(msg.payload())?;
                    if ack["command_id"] == id.as_str() && ack["phase"] != "received" {
                        return anyhow::Ok(ack["phase"].as_str().unwrap_or_default().to_string());
                    }
                }
                bail!("ack stream closed")
            })
            .await;
            if let Ok(Ok(phase)) = phase
                && phase == "done"
            {
                return anyhow::Ok(());
            }
        }
        unreachable!()
    })
    .await;

    // Assert — the module dispatched, each PCS took its 100 kW half, and the
    // 1 Hz rebalance tick kept it there rather than dropping it to 0 W
    done.map_err(|_| anyhow::anyhow!("bess_module_dc never dispatched"))??;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let half = vec![0x0001, 0x86A0];
    for pcs in [&pcs_1, &pcs_2] {
        let writes: Vec<Vec<u16>> = pcs
            .writes_at(at::CMD_REAL_PWR)
            .into_iter()
            .map(|w| w.words)
            .collect();
        let from = writes
            .iter()
            .position(|w| *w == half)
            .expect("never took its half");
        assert!(writes[from..].iter().all(|w| *w == half), "{writes:?}");
    }
    cancel.cancel();
    gateway.await??;
    Ok(())
}
