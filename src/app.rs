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
use crate::der_dispatch;
use crate::dispatch;
use crate::dnp3::client as dnp3;
use crate::envelope;
use crate::http::client::fetch_asyncapi;
use crate::modbus::client as modbus;
use crate::mqtt::{publisher, subscriber};
use crate::redfish::client as redfish;
use crate::snmp::client as snmp;
use crate::synthetic::{self, Computation, InputCache, Operation, SyntheticTaskConfig};
use anyhow::{Context, Result};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
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
    // from cfg (static `arcnode_gateway` per platform File-RBAC). On-prem
    // cloud-customer deployments don't set the env var — the gateway fetches
    // it from the stack's Secrets Manager entry instead (AWS env creds).
    let mqtt_password = match std::env::var("MQTT_GATEWAY_PASSWORD") {
        Ok(pw) => pw,
        Err(_) => crate::bootstrap::fetch_gateway_password()
            .await
            .context("MQTT_GATEWAY_PASSWORD unset — broker auth requires it")?,
    };
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

    // Synthetic-channel inputs + distribute-binding children's cache-backed
    // topics (operating_state/state_of_charge — read at dispatch time, not
    // polled). Subscribed alongside the beacon so the single dispatcher
    // routes all three. Reconcile-time additions are NOT dynamically
    // resubscribed today; topology changes that introduce new ones need a
    // gateway restart (logged + tracked in handoff).
    let mut input_topics = collect_synthetic_input_topics(&initial_spec, &cfg.site_id);
    input_topics.extend(collect_distribute_input_topics(&initial_spec, &cfg.site_id));
    input_topics.extend(collect_der_dispatch_active_power_topics(
        &initial_spec,
        &cfg.site_id,
    ));
    input_topics.extend(collect_der_dispatch_state_of_charge_topics(
        &initial_spec,
        &cfg.site_id,
    ));
    // der_dispatch's own target-side channels (published by ems-der-control-
    // api) — the site-distribution task's reactive trigger. Not templated
    // (der_dispatch isn't in every spec), so subscribed unconditionally;
    // holds forever on a site with no der_dispatch, same as its other inputs.
    input_topics.push(format!(
        "sites/{}/devices/der_dispatch/measurements/target_active_power/watts",
        cfg.site_id
    ));
    input_topics.push(format!(
        "sites/{}/devices/der_dispatch/measurements/event_active/none",
        cfg.site_id
    ));
    let cache = synthetic::new_input_cache();
    // Device/channel bindings backing dispatch — refreshed on every
    // successful spec re-fetch so accepts/rejects/writes track live topology.
    let device_channels_map = Arc::new(RwLock::new(device_channels(&initial_spec)));
    let device_trust_map = Arc::new(RwLock::new(initial_spec.x_device_trust.clone()));
    // Last real operator/dispatcher setpoint per (device, command) — the
    // envelope actuation loop's ramp-back target. Empty at boot; populated
    // as real commands arrive. Not reset on reconcile (a topology refresh
    // shouldn't forget what an operator most recently asked for).
    let last_requested: dispatch::LastRequestedSetpoints = Arc::new(RwLock::new(HashMap::new()));
    let mut beacon_rx = subscriber::subscribe(
        &mut client,
        &input_topics,
        cache.clone(),
        &cfg.site_id,
        device_channels_map.clone(),
        device_trust_map.clone(),
        cfg.gateway_credentials.clone(),
        last_requested.clone(),
    )
    .await?;

    let (mut task_handles, mut task_cancel) = spawn_task_set(
        &initial_spec,
        &cfg,
        client.clone(),
        cache.clone(),
        last_requested.clone(),
        device_channels_map.clone(),
        device_trust_map.clone(),
    );

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
                            spawn_task_set(&initial_spec, &cfg, client.clone(), cache.clone(), last_requested.clone(), device_channels_map.clone(), device_trust_map.clone());
                        task_handles = h;
                        task_cancel = c;
                        continue;
                    }
                };
                info!(version = %fresh.info.version, "spec re-fetched");
                *device_channels_map.write().await = device_channels(&fresh);
                *device_trust_map.write().await = fresh.x_device_trust.clone();
                if let Err(e) =
                    validate_trust_creds_alignment(&fresh, cfg.gateway_credentials.as_ref())
                {
                    warn!(error = %e, "new spec fails trust/creds alignment; keeping current task set");
                    let (h, c) =
                        spawn_task_set(&initial_spec, &cfg, client.clone(), cache.clone(), last_requested.clone(), device_channels_map.clone(), device_trust_map.clone());
                    task_handles = h;
                    task_cancel = c;
                    continue;
                }
                let (h, c) = spawn_task_set(&fresh, &cfg, client.clone(), cache.clone(), last_requested.clone(), device_channels_map.clone(), device_trust_map.clone());
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

/// Per-device, per-`{verb}_{target}` binding map projected from the spec's
/// x-command-source — the dispatch lookup `dispatch::handle_command` uses to
/// resolve an inbound command topic (which carries verb+target, not the
/// template's channel name) to its binding.
fn device_channels(spec: &AsyncApiSpec) -> HashMap<String, HashMap<String, ProtocolBinding>> {
    spec.x_command_source
        .iter()
        .map(|(device_id, commands)| {
            let bindings = commands
                .values()
                .map(|cmd| {
                    let key = format!("{}_{}", cmd.verb, cmd.target);
                    (key, clone_binding(&cmd.binding))
                })
                .collect();
            (device_id.clone(), bindings)
        })
        .collect()
}

/// Walk the spec's x-protocol-source and spawn one task per
/// (device, measurement) tuple. Synthetic bindings get their own loop
/// (no south-side poll); all others go through the protocol-poll path.
/// Returns a `JoinSet` of handles and the parent `CancellationToken` used to
/// stop them en masse on reconcile.
#[allow(clippy::too_many_arguments)]
fn spawn_task_set(
    spec: &AsyncApiSpec,
    cfg: &Config,
    client: paho_mqtt::AsyncClient,
    cache: InputCache,
    last_requested: dispatch::LastRequestedSetpoints,
    device_channels: Arc<RwLock<HashMap<String, HashMap<String, ProtocolBinding>>>>,
    device_trust: Arc<RwLock<HashMap<String, DeviceTrust>>>,
) -> (JoinSet<()>, CancellationToken) {
    let parent = CancellationToken::new();
    let mut handles = JoinSet::new();
    let mut spawned_poll = 0usize;
    let mut spawned_synthetic = 0usize;
    let mut spawned_envelope = 0usize;
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
    for (device_id, commands) in &spec.x_command_source {
        for source in commands.values() {
            let ProtocolBinding::Distribute(d) = &source.binding else {
                continue;
            };
            // Every distribute binding gets a rebalance task — guarded ones
            // also get the envelope clamp on top; a plain distribute just
            // rebalances its child split on drift with no clamp.
            let guard = envelope::envelope_guard_config(d).map(|guard_config| {
                envelope::EnvelopeGuardConfig {
                    import_limit_topic: substitute_site_id(
                        &guard_config.import_limit_topic,
                        &cfg.site_id,
                    ),
                    export_limit_topic: substitute_site_id(
                        &guard_config.export_limit_topic,
                        &cfg.site_id,
                    ),
                    active_power_topic: substitute_site_id(
                        &guard_config.active_power_topic,
                        &cfg.site_id,
                    ),
                    ..guard_config
                }
            });
            let guarded = guard.is_some();
            let channel_key = format!("{}_{}", source.verb, source.target);
            let task_cfg = envelope::EnvelopeTaskConfig {
                device_id: device_id.clone(),
                channel_key: channel_key.clone(),
                guard,
                binding: clone_binding(&source.binding),
                tick_hz: DEFAULT_POLL_HZ,
            };
            let handle = envelope::task::spawn(
                task_cfg,
                cfg.site_id.clone(),
                cache.clone(),
                last_requested.clone(),
                device_channels.clone(),
                device_trust.clone(),
                cfg.gateway_credentials.clone(),
                parent.child_token(),
            );
            handles.spawn(async move {
                let _ = handle.await;
            });
            spawned_envelope += 1;
            info!(%device_id, %channel_key, guarded, "distribute rebalance task spawned");
        }
    }

    let der_dispatch_source_topics = collect_der_dispatch_active_power_topics(spec, &cfg.site_id);
    let der_dispatch_cfg = der_dispatch::DerDispatchTaskConfig {
        output_topic: format!(
            "sites/{}/devices/der_dispatch/measurements/actual_active_power/watts",
            cfg.site_id
        ),
        source_topics: der_dispatch_source_topics,
        tick_hz: DEFAULT_POLL_HZ,
    };
    let der_dispatch_handle = der_dispatch::spawn(
        der_dispatch_cfg,
        cache.clone(),
        client.clone(),
        parent.child_token(),
    );
    handles.spawn(async move {
        let _ = der_dispatch_handle.await;
    });

    let site_distribution_cfg = der_dispatch::SiteDistributionConfig {
        site_id: cfg.site_id.clone(),
        target_topic: format!(
            "sites/{}/devices/der_dispatch/measurements/target_active_power/watts",
            cfg.site_id
        ),
        event_active_topic: format!(
            "sites/{}/devices/der_dispatch/measurements/event_active/none",
            cfg.site_id
        ),
        tick_hz: DEFAULT_POLL_HZ,
    };
    let site_distribution_handle = der_dispatch::spawn_site_distribution(
        site_distribution_cfg,
        cache.clone(),
        client.clone(),
        device_channels.clone(),
        parent.child_token(),
    );
    handles.spawn(async move {
        let _ = site_distribution_handle.await;
    });

    info!(
        spawned_poll,
        spawned_synthetic, spawned_envelope, "task set built"
    );
    (handles, parent)
}

/// Build a `SyntheticTaskConfig` and spawn the loop. Returns None if the
/// operation name is unknown (logged and the channel is dropped — the gateway
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
    let computation = if binding.operation == "weighted_mean" {
        let pairs = binding
            .pairs
            .iter()
            .map(|p| (substitute_site_id(&p.topic, site_id), p.weight))
            .collect();
        Computation::WeightedMean { pairs }
    } else {
        let operation = match Operation::parse(&binding.operation) {
            Ok(f) => f,
            Err(err) => {
                warn!(output_topic, error = %err, "synthetic operation parse failed; dropping channel");
                return None;
            }
        };
        let input_topics = binding
            .inputs
            .iter()
            .map(|t| substitute_site_id(t, site_id))
            .collect();
        Computation::Operation {
            operation,
            input_topics,
        }
    };
    let cfg = SyntheticTaskConfig {
        output_topic: output_topic.to_string(),
        computation,
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
                for pair in &b.pairs {
                    topics.insert(substitute_site_id(&pair.topic, site_id));
                }
            }
        }
    }
    topics.into_iter().collect()
}

/// Walk the spec's x-command-source for `distribute` bindings and collect
/// each child's `operating_state_topic`/`state_of_charge_topic` (with
/// `{site_id}` substituted). These are read from the cache at dispatch time,
/// not polled — the gateway still needs to be subscribed for them to ever
/// land in the cache.
fn collect_distribute_input_topics(spec: &AsyncApiSpec, site_id: &str) -> Vec<String> {
    let mut topics: BTreeSet<String> = BTreeSet::new();
    for commands in spec.x_command_source.values() {
        for source in commands.values() {
            if let ProtocolBinding::Distribute(d) = &source.binding {
                for child in &d.children {
                    topics.insert(substitute_site_id(&child.operating_state_topic, site_id));
                    topics.insert(substitute_site_id(&child.state_of_charge_topic, site_id));
                }
                if let Some(guard) = envelope::envelope_guard_config(d) {
                    topics.insert(substitute_site_id(&guard.import_limit_topic, site_id));
                    topics.insert(substitute_site_id(&guard.export_limit_topic, site_id));
                    topics.insert(substitute_site_id(&guard.active_power_topic, site_id));
                }
            }
        }
    }
    topics.into_iter().collect()
}

/// Every distribute-parent device_id — today, every `bess_module` instance.
/// A device qualifies by having at least one `Distribute`-bound command, not
/// by template name — stays correct at any module count and at any
/// distribution tier (module→rack today, site→module built on the same
/// detection).
fn distribute_parent_device_ids(spec: &AsyncApiSpec) -> Vec<String> {
    spec.x_command_source
        .iter()
        .filter(|(_, commands)| {
            commands
                .values()
                .any(|source| matches!(source.binding, ProtocolBinding::Distribute(_)))
        })
        .map(|(device_id, _)| device_id.clone())
        .collect()
}

/// Each distribute-parent's own `active_power` topic (with `{site_id}`
/// substituted) — feeds `der_dispatch::actual_active_power`'s site-total
/// sum. Deliberately does NOT include state_of_charge: that's a different
/// task's (`site_distribution`) concern, and mixing it into this list would
/// make the sum wait on an unrelated measurement that may never publish.
fn collect_der_dispatch_active_power_topics(spec: &AsyncApiSpec, site_id: &str) -> Vec<String> {
    distribute_parent_device_ids(spec)
        .iter()
        .map(|device_id| {
            substitute_site_id(
                &format!("sites/{{site_id}}/devices/{device_id}/measurements/active_power/watts"),
                site_id,
            )
        })
        .collect()
}

/// Each distribute-parent's own `state_of_charge` topic (with `{site_id}`
/// substituted) — not summed by anything, just needs to be subscribed so
/// `site_distribution`'s SoC-weighted split has it cached.
fn collect_der_dispatch_state_of_charge_topics(spec: &AsyncApiSpec, site_id: &str) -> Vec<String> {
    distribute_parent_device_ids(spec)
        .iter()
        .map(|device_id| {
            substitute_site_id(
                &format!(
                    "sites/{{site_id}}/devices/{device_id}/measurements/state_of_charge/percent"
                ),
                site_id,
            )
        })
        .collect()
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
        // MQTT subscriptions + operation evaluation); never reached via the
        // single-point poll path. Unreachable acts as a tripwire if the
        // dispatcher upstream forgets to route synthetic channels separately.
        ProtocolBinding::Synthetic(_) => {
            unreachable!("synthetic bindings are driven by the synthetic module, not read_value")
        }
        // Distribute is command-only — it lives in x-command-source, never
        // x-protocol-source, so the measurement poll path never sees one.
        ProtocolBinding::Distribute(_) => {
            unreachable!("distribute bindings are commands, never a measurement source")
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
        BacnetIpBinding, BacnetScBinding, DistributeBinding, Dnp3TcpBinding, ModbusTcpBinding,
        RedfishBinding, SnmpBinding, SyntheticBinding,
    };
    match b {
        ProtocolBinding::ModbusTcp(m) => ProtocolBinding::ModbusTcp(ModbusTcpBinding {
            host: m.host.clone(),
            port: m.port,
            unit_id: m.unit_id.clone(),
            address: m.address,
            scale: m.scale,
            offset: m.offset,
            data_type: m.data_type,
            word_order: m.word_order,
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
            operation: s.operation.clone(),
            inputs: s.inputs.clone(),
            pairs: s.pairs.clone(),
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
        ProtocolBinding::Distribute(d) => ProtocolBinding::Distribute(DistributeBinding {
            allocation_policy: d.allocation_policy.clone(),
            children: d.children.clone(),
            ramp_rate_per_sec: d.ramp_rate_per_sec,
            hysteresis_margin: d.hysteresis_margin,
            hysteresis_dwell_secs: d.hysteresis_dwell_secs,
            power_min: d.power_min,
            power_max: d.power_max,
            import_limit_topic: d.import_limit_topic.clone(),
            export_limit_topic: d.export_limit_topic.clone(),
            active_power_topic: d.active_power_topic.clone(),
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
