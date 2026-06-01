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

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::{AsyncApiSpec, ProtocolBinding, SyntheticBinding};
use crate::bacnet::client as bacnet;
use crate::bacnet_sc::client as bacnet_sc;
use crate::config::{Config, GatewayCredentials};
use crate::dnp3::client as dnp3;
use crate::http::client::fetch_asyncapi;
use crate::modbus::client as modbus;
use crate::mqtt::{publisher, subscriber};
use crate::redfish::client as redfish;
use crate::snmp::client as snmp;
use crate::synthetic::{self, Formula, InputCache, SyntheticTaskConfig};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
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

    // Password is a SECRET — env-loaded, never in cfg.yml. Username comes
    // from cfg (static `arcnode_gateway` per platform File-RBAC).
    let mqtt_password = std::env::var("MQTT_GATEWAY_PASSWORD")
        .context("MQTT_GATEWAY_PASSWORD env var unset — broker auth requires it")?;
    let mut client = publisher::connect(
        &cfg.broker_url,
        "ems-industrial-gateway",
        &cfg.mqtt_username,
        &mqtt_password,
    )
    .await?;
    // Fetch the spec first so we know which input topics to subscribe to.
    let initial_spec = fetch_asyncapi(&cfg.device_api_url).await?;
    info!(version = %initial_spec.info.version, "initial spec fetched");
    // Fail-fast at boot if any device requires tls_mutual but mTLS creds are
    // unconfigured. Security regression should be loud, not silent.
    validate_trust_creds_alignment(&initial_spec, cfg.gateway_credentials.as_ref())?;

    // Synthetic-channel input topics. Subscribed alongside the beacon so the
    // single dispatcher routes both. Reconcile-time additions are NOT
    // dynamically resubscribed today; topology changes that introduce NEW
    // synthetic inputs need a gateway restart (logged + tracked in handoff).
    let input_topics = collect_synthetic_input_topics(&initial_spec, &cfg.site_id);
    let cache = synthetic::new_input_cache();
    let mut beacon_rx = subscriber::subscribe(&mut client, &input_topics, cache.clone()).await?;

    let (mut task_handles, mut task_cancel) =
        spawn_task_set(&initial_spec, &cfg, client.clone(), cache.clone());

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
                        let (h, c) =
                            spawn_task_set(&initial_spec, &cfg, client.clone(), cache.clone());
                        task_handles = h;
                        task_cancel = c;
                        continue;
                    }
                };
                info!(version = %fresh.info.version, "spec re-fetched");
                if let Err(e) =
                    validate_trust_creds_alignment(&fresh, cfg.gateway_credentials.as_ref())
                {
                    warn!(error = %e, "new spec fails trust/creds alignment; keeping current task set");
                    let (h, c) =
                        spawn_task_set(&initial_spec, &cfg, client.clone(), cache.clone());
                    task_handles = h;
                    task_cancel = c;
                    continue;
                }
                let (h, c) = spawn_task_set(&fresh, &cfg, client.clone(), cache.clone());
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
/// (device, measurement) tuple. Synthetic bindings get their own loop
/// (no south-side poll); all others go through the protocol-poll path.
/// Returns a `JoinSet` of handles and the parent `CancellationToken` used to
/// stop them en masse on reconcile.
fn spawn_task_set(
    spec: &AsyncApiSpec,
    cfg: &Config,
    client: paho_mqtt::AsyncClient,
    cache: InputCache,
) -> (JoinSet<()>, CancellationToken) {
    let parent = CancellationToken::new();
    let mut handles = JoinSet::new();
    let mut spawned_poll = 0usize;
    let mut spawned_synthetic = 0usize;
    for (device_id, channels) in &spec.x_protocol_source {
        for (measurement, source) in channels {
            let task_cancel = parent.child_token();
            let topic = build_topic(&cfg.site_id, device_id, measurement, &source.unit);
            let poll_rate = clamp_poll_rate(source.poll_rate_hz, &topic);
            if let ProtocolBinding::Synthetic(b) = &source.binding {
                if let Some(handle) = spawn_synthetic(
                    b,
                    &topic,
                    poll_rate,
                    &cfg.site_id,
                    cache.clone(),
                    client.clone(),
                    task_cancel.clone(),
                ) {
                    handles.spawn(async move {
                        // The synthetic spawn returns its own JoinHandle; await
                        // it inside this JoinSet entry so shutdown completes
                        // when the loop exits on its cancel token.
                        let _ = handle.await;
                    });
                    spawned_synthetic += 1;
                    info!(%device_id, %measurement, %topic, poll_rate, "synthetic task spawned");
                }
                continue;
            }
            let binding = clone_binding(&source.binding);
            // Reason: trust is per-device (x-device-trust[device_id]); clone
            // into the task so the spawned future owns it for its full life.
            let trust = spec.x_device_trust.get(device_id).cloned();
            // Gateway credentials are global — same Option for every task.
            let creds = cfg.gateway_credentials.clone();
            let topic_for_task = topic.clone();
            let client_for_task = client.clone();
            handles.spawn(async move {
                run_task(
                    binding,
                    topic_for_task,
                    poll_rate,
                    client_for_task,
                    task_cancel,
                    trust,
                    creds,
                )
                .await;
            });
            spawned_poll += 1;
            info!(%device_id, %measurement, %topic, poll_rate, "poll task spawned");
        }
    }
    info!(spawned_poll, spawned_synthetic, "task set built");
    (handles, parent)
}

/// Build a `SyntheticTaskConfig` and spawn the loop. Returns None if the
/// formula name is unknown (logged and the channel is dropped — the gateway
/// keeps running for valid channels).
#[allow(clippy::too_many_arguments)]
fn spawn_synthetic(
    binding: &SyntheticBinding,
    output_topic: &str,
    tick_hz: f64,
    site_id: &str,
    cache: InputCache,
    mqtt: paho_mqtt::AsyncClient,
    cancel: CancellationToken,
) -> Option<tokio::task::JoinHandle<()>> {
    let formula = match Formula::parse(&binding.formula) {
        Ok(f) => f,
        Err(err) => {
            warn!(output_topic, error = %err, "synthetic formula parse failed; dropping channel");
            return None;
        }
    };
    let input_topics: Vec<String> = binding
        .inputs
        .iter()
        .map(|t| substitute_site_id(t, site_id))
        .collect();
    let cfg = SyntheticTaskConfig {
        output_topic: output_topic.to_string(),
        input_topics,
        formula,
        tick_hz,
    };
    Some(synthetic::task::spawn(cfg, cache, mqtt, cancel))
}

/// Walk the spec for synthetic bindings + collect the unique set of input
/// topics (with `{site_id}` substituted). Used to subscribe up-front so cached
/// values are available by the time synthetic tasks tick.
fn collect_synthetic_input_topics(spec: &AsyncApiSpec, site_id: &str) -> Vec<String> {
    let mut topics: BTreeSet<String> = BTreeSet::new();
    for channels in spec.x_protocol_source.values() {
        for source in channels.values() {
            if let ProtocolBinding::Synthetic(b) = &source.binding {
                for raw in &b.inputs {
                    topics.insert(substitute_site_id(raw, site_id));
                }
            }
        }
    }
    topics.into_iter().collect()
}

/// Substitute `{site_id}` in an input topic template. `{device_id}` is
/// already resolved by ems-device-api at AsyncAPI generation time.
fn substitute_site_id(template: &str, site_id: &str) -> String {
    template.replace("{site_id}", site_id)
}

/// One per-measurement loop. Ticks at `poll_rate_hz`, reads via the protocol
/// client, publishes the value to MQTT. On read error, logs warn and waits
/// for the next tick (no double-retry — the protocol client already retries
/// internally).
#[allow(clippy::too_many_arguments)]
async fn run_task(
    binding: ProtocolBinding,
    topic: String,
    poll_rate_hz: f64,
    client: paho_mqtt::AsyncClient,
    cancel: CancellationToken,
    trust: Option<DeviceTrust>,
    creds: Option<GatewayCredentials>,
) {
    let period = Duration::from_secs_f64(1.0 / poll_rate_hz);
    let mut ticker = interval(period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => {
                match read_value(&binding, trust.as_ref(), creds.as_ref()).await {
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
/// `ProtocolBinding` variant lands. `trust` carries the device's
/// `x-device-trust` block (looked up by device_id at spawn time). `creds`
/// is the gateway's global mTLS material (CA bundle + cert + key paths).
async fn read_value(
    binding: &ProtocolBinding,
    trust: Option<&DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<f64> {
    match binding {
        ProtocolBinding::ModbusTcp(b) => modbus::read_measurement(b, trust, creds).await,
        ProtocolBinding::Snmp(b) => snmp::read_measurement(b, trust, creds).await,
        ProtocolBinding::Redfish(b) => redfish::read_measurement(b, trust, creds).await,
        ProtocolBinding::Dnp3Tcp(b) => dnp3::read_measurement(b, trust, creds).await,
        ProtocolBinding::BacnetIp(b) => bacnet::read_measurement(b, trust, creds).await,
        ProtocolBinding::BacnetSc(b) => bacnet_sc::read_measurement(b, trust, creds).await,
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
        BacnetIpBinding, BacnetScBinding, Dnp3TcpBinding, ModbusTcpBinding, RedfishBinding,
        SnmpBinding, SyntheticBinding,
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
        ProtocolBinding::BacnetSc(b) => ProtocolBinding::BacnetSc(BacnetScBinding {
            hub_url: b.hub_url.clone(),
            device_vmac: b.device_vmac.clone(),
            object_type: b.object_type.clone(),
            object_instance: b.object_instance,
            property_id: b.property_id.clone(),
        }),
    }
}

/// Boot/reconcile guard: every device declaring `tls_mutual` trust needs the
/// gateway to have mTLS material configured. Fail-fast on misalignment so a
/// missing mount isn't silently downgraded to plain TCP. Called once before
/// the initial spawn and once per reconcile.
fn validate_trust_creds_alignment(
    spec: &AsyncApiSpec,
    creds: Option<&GatewayCredentials>,
) -> Result<()> {
    if creds.is_some() {
        return Ok(());
    }
    // Reason: only TLS-using variants need gateway_credentials; SNMPv3 USM
    // is HMAC-based and authenticates via env-var passphrases, not PKI.
    let offender = spec
        .x_device_trust
        .iter()
        .find(|(_, trust)| matches!(trust, DeviceTrust::TlsMutual { .. }))
        .map(|(device_id, _)| device_id);
    if let Some(device_id) = offender {
        anyhow::bail!(
            "device {device_id} requires tls_mutual but gateway_credentials is unset; \
             mount the CA bundle + gateway cert/key and set cfg.gateway_credentials",
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// Minimal spec helper — fills in mandatory fields, lets the caller hand
    /// us just the trust block under test.
    fn spec_with_trust(trust: HashMap<String, DeviceTrust>) -> AsyncApiSpec {
        let json = r#"{
            "info": { "version": "v1" },
            "x-protocol-source": {}
        }"#;
        let mut spec: AsyncApiSpec = serde_json::from_str(json).unwrap();
        spec.x_device_trust = trust;
        spec
    }

    fn creds() -> GatewayCredentials {
        GatewayCredentials {
            ca_bundle_path: PathBuf::from("/etc/secrets/ca.crt"),
            cert_path: PathBuf::from("/etc/secrets/gw.crt"),
            key_path: PathBuf::from("/etc/secrets/gw.key"),
        }
    }

    #[test]
    fn validate_errors_when_tls_required_but_creds_absent() {
        // Arrange — one device asks for TLS, gateway has no creds.
        let mut trust = HashMap::new();
        trust.insert(
            "meter_01".to_string(),
            DeviceTrust::TlsMutual {
                subject_name: "meter-01".into(),
            },
        );
        let spec = spec_with_trust(trust);
        // Act
        let result = validate_trust_creds_alignment(&spec, None);
        // Assert — fail-fast with a clear error mentioning the device
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("meter_01"),
            "error should name the offending device, got: {err}"
        );
    }

    #[test]
    fn validate_ok_when_tls_required_and_creds_present() {
        let mut trust = HashMap::new();
        trust.insert(
            "meter_01".to_string(),
            DeviceTrust::TlsMutual {
                subject_name: "meter-01".into(),
            },
        );
        let spec = spec_with_trust(trust);
        let creds = creds();
        assert!(validate_trust_creds_alignment(&spec, Some(&creds)).is_ok());
    }

    #[test]
    fn validate_ok_when_no_tls_required_and_creds_absent() {
        // Back-compat: pre-PKI spec (empty trust block) should not require creds.
        let spec = spec_with_trust(HashMap::new());
        assert!(validate_trust_creds_alignment(&spec, None).is_ok());
    }
}
