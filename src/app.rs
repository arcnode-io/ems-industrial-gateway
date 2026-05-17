//! Boot orchestration. Tier 2: spec-driven continuous reads.
//!
//! Boot order:
//! 1. Connect to MQTT broker
//! 2. Subscribe to `system/topology_changed` (watch::Receiver)
//! 3. Fetch `/asyncapi` once for the initial spec
//! 4. Spawn one tokio task per (device, measurement) — each owns its own
//!    `interval` and a child `CancellationToken`.
//! 5. Loop `select! { beacon changed => respawn all, cancel => break }`.
//!    On respawn: cancel + join existing tasks, re-fetch spec, build new set.
//! 6. On exit: disconnect MQTT cleanly.

use crate::asyncapi::types::{AsyncApiSpec, ProtocolBinding};
use crate::bacnet::client as bacnet;
use crate::config::Config;
use crate::dnp3::client as dnp3;
use crate::http::client::fetch_asyncapi;
use crate::modbus::client as modbus;
use crate::mqtt::{publisher, subscriber};
use crate::redfish::client as redfish;
use crate::snmp::client as snmp;
use anyhow::{Context, Result};
use std::time::Duration;
use tokio::task::JoinSet;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Minimum allowed poll rate (slowest). Below this, tasks are effectively dead.
const MIN_POLL_HZ: f64 = 0.01;
/// Maximum allowed poll rate (fastest). Above this risks melting the device.
const MAX_POLL_HZ: f64 = 10.0;
/// Default poll rate when the spec author omits `poll_rate_hz`.
const DEFAULT_POLL_HZ: f64 = 1.0;

/// Tier 2 flow. Returns when `cancel` fires (SIGINT/SIGTERM in prod, test
/// driver in tests). Errors propagate from initial setup; per-task read
/// failures are logged and skipped (task keeps ticking).
pub async fn run(cfg: Config, cancel: CancellationToken) -> Result<()> {
    info!(
        device_api_url = %cfg.device_api_url,
        broker_url = %cfg.broker_url,
        site_id = %cfg.site_id,
        "gateway starting",
    );

    let mut client = publisher::connect(&cfg.broker_url, "ems-industrial-gateway").await?;
    let mut beacon_rx = subscriber::subscribe_topology_changed(&mut client).await?;
    let initial_spec = fetch_asyncapi(&cfg.device_api_url).await?;
    info!(version = %initial_spec.info.version, "initial spec fetched");

    let (mut task_handles, mut task_cancel) = spawn_task_set(&initial_spec, &cfg, client.clone());

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("shutdown signal received");
                break;
            }
            res = beacon_rx.changed() => {
                if res.is_err() {
                    warn!("beacon channel closed — exiting");
                    break;
                }
                info!("reconciling on topology beacon");
                task_cancel.cancel();
                while task_handles.join_next().await.is_some() {}
                let fresh = match fetch_asyncapi(&cfg.device_api_url).await {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(error = %e, "respawn fetch failed; keeping current task set");
                        // Re-spawn the old set so we don't end up idle.
                        let (h, c) = spawn_task_set(&initial_spec, &cfg, client.clone());
                        task_handles = h;
                        task_cancel = c;
                        continue;
                    }
                };
                info!(version = %fresh.info.version, "spec re-fetched");
                let (h, c) = spawn_task_set(&fresh, &cfg, client.clone());
                task_handles = h;
                task_cancel = c;
            }
        }
    }

    task_cancel.cancel();
    while task_handles.join_next().await.is_some() {}
    client.disconnect(None).await.context("mqtt disconnect")?;
    info!("gateway stopped");
    Ok(())
}

/// Walk the spec's x-protocol-source and spawn one task per
/// (device, measurement) tuple. Returns a `JoinSet` of handles and the parent
/// `CancellationToken` used to stop them en masse on reconcile.
fn spawn_task_set(
    spec: &AsyncApiSpec,
    cfg: &Config,
    client: paho_mqtt::AsyncClient,
) -> (JoinSet<()>, CancellationToken) {
    let parent = CancellationToken::new();
    let mut handles = JoinSet::new();
    let mut spawned = 0usize;
    for (device_id, channels) in &spec.x_protocol_source {
        for (measurement, source) in channels {
            let task_cancel = parent.child_token();
            let topic = build_topic(&cfg.site_id, device_id, measurement, &source.unit);
            let poll_rate = clamp_poll_rate(source.poll_rate_hz, &topic);
            let binding = clone_binding(&source.binding);
            let topic_for_task = topic.clone();
            let client_for_task = client.clone();
            handles.spawn(async move {
                run_task(
                    binding,
                    topic_for_task,
                    poll_rate,
                    client_for_task,
                    task_cancel,
                )
                .await;
            });
            spawned += 1;
            info!(%device_id, %measurement, %topic, poll_rate, "task spawned");
        }
    }
    info!(spawned, "task set built");
    (handles, parent)
}

/// One per-measurement loop. Ticks at `poll_rate_hz`, reads via the protocol
/// client, publishes the value to MQTT. On read error, logs warn and waits
/// for the next tick (no double-retry — the protocol client already retries
/// internally).
async fn run_task(
    binding: ProtocolBinding,
    topic: String,
    poll_rate_hz: f64,
    client: paho_mqtt::AsyncClient,
    cancel: CancellationToken,
) {
    let period = Duration::from_secs_f64(1.0 / poll_rate_hz);
    let mut ticker = interval(period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => {
                match read_value(&binding).await {
                    Ok(value) => {
                        if let Err(e) =
                            publisher::publish_measurement(&client, &topic, value).await
                        {
                            warn!(%topic, error = %e, "publish failed");
                        }
                    }
                    Err(e) => warn!(%topic, error = %e, "read failed; skipping tick"),
                }
            }
        }
    }
}

/// Single-point protocol dispatch. Add a `match` arm when a new
/// `ProtocolBinding` variant lands.
async fn read_value(binding: &ProtocolBinding) -> Result<f64> {
    match binding {
        ProtocolBinding::ModbusTcp(b) => modbus::read_measurement(b).await,
        ProtocolBinding::Snmp(b) => snmp::read_measurement(b).await,
        ProtocolBinding::Redfish(b) => redfish::read_measurement(b).await,
        ProtocolBinding::Dnp3Tcp(b) => dnp3::read_measurement(b).await,
        ProtocolBinding::BacnetIp(b) => bacnet::read_measurement(b).await,
        // Synthetic channels are driven by `src/synthetic/` (own loop with
        // MQTT subscriptions + formula evaluation); never reached via the
        // single-point poll path. Unreachable acts as a tripwire if the
        // dispatcher upstream forgets to route synthetic channels separately.
        ProtocolBinding::Synthetic(_) => {
            unreachable!("synthetic bindings are driven by the synthetic module, not read_value")
        }
    }
}

/// Build the MQTT topic per ADR-002 §2 measurement address shape.
fn build_topic(site_id: &str, device_id: &str, measurement: &str, unit: &str) -> String {
    format!("sites/{site_id}/devices/{device_id}/measurements/{measurement}/{unit}")
}

/// Apply the gateway's poll-rate policy: null → default, otherwise clamp.
/// Logs a warn if clamping triggers so DTM authors can tune.
fn clamp_poll_rate(value: Option<f64>, topic: &str) -> f64 {
    let raw = value.unwrap_or(DEFAULT_POLL_HZ);
    if raw < MIN_POLL_HZ {
        warn!(%topic, raw, "poll_rate_hz below MIN; clamping");
        return MIN_POLL_HZ;
    }
    if raw > MAX_POLL_HZ {
        warn!(%topic, raw, "poll_rate_hz above MAX; clamping");
        return MAX_POLL_HZ;
    }
    raw
}

/// Clone a `ProtocolBinding` for task ownership. Variants hold owned `String`
/// fields; manual clone is cheap and keeps the binding `!Clone` for the rest
/// of the code (forcing intentional copies here only).
fn clone_binding(b: &ProtocolBinding) -> ProtocolBinding {
    use crate::asyncapi::types::{
        BacnetIpBinding, Dnp3TcpBinding, ModbusTcpBinding, RedfishBinding, SnmpBinding,
        SyntheticBinding,
    };
    match b {
        ProtocolBinding::ModbusTcp(m) => ProtocolBinding::ModbusTcp(ModbusTcpBinding {
            host: m.host.clone(),
            port: m.port,
            unit_id: m.unit_id.clone(),
            address: m.address,
            scale: m.scale,
            offset: m.offset,
        }),
        ProtocolBinding::Snmp(s) => ProtocolBinding::Snmp(SnmpBinding {
            host: s.host.clone(),
            port: s.port,
            oid: s.oid.clone(),
        }),
        ProtocolBinding::Redfish(r) => ProtocolBinding::Redfish(RedfishBinding {
            host: r.host.clone(),
            port: r.port,
            uri: r.uri.clone(),
            json_pointer: r.json_pointer.clone(),
        }),
        ProtocolBinding::Dnp3Tcp(d) => ProtocolBinding::Dnp3Tcp(Dnp3TcpBinding {
            host: d.host.clone(),
            port: d.port,
            point_index: d.point_index,
            point_type: d.point_type.clone(),
            variation: d.variation,
        }),
        ProtocolBinding::Synthetic(s) => ProtocolBinding::Synthetic(SyntheticBinding {
            formula: s.formula.clone(),
            inputs: s.inputs.clone(),
        }),
        ProtocolBinding::BacnetIp(b) => ProtocolBinding::BacnetIp(BacnetIpBinding {
            host: b.host.clone(),
            port: b.port,
            device_instance: b.device_instance,
            object_type: b.object_type.clone(),
            object_instance: b.object_instance,
            property_id: b.property_id.clone(),
        }),
    }
}
