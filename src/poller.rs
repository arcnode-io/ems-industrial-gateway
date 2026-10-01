//! One poller per device. Each device's readings are read one after
//! another, each when its own poll period comes due, so a device sees at
//! most one request at a time however many readings it has.
//!
//! Within a tick, readings that share a Redfish resource share one fetch,
//! and a device keeps one Modbus session for its life instead of one per
//! read.
//!
//! Reason: a task per reading meant a device with N readings got N
//! concurrent requests. Across ~100 GPU nodes that was ~350 connections to
//! one service, which exhausted its file descriptors; a real BMC allows a
//! handful of sessions and refuses the rest.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::ProtocolBinding;
use crate::bacnet::client as bacnet;
use crate::bacnet_sc::client as bacnet_sc;
use crate::config::GatewayCredentials;
use crate::dnp3::client as dnp3;
use crate::modbus::client as modbus;
use crate::mqtt::publisher;
use crate::redfish::client as redfish;
use crate::snmp::client as snmp;
use anyhow::Result;
use rodbus::client::Channel;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// One reading a device poller owns.
pub struct Point {
    /// MQTT topic the reading publishes to.
    pub topic: String,
    /// How to read it.
    pub binding: ProtocolBinding,
    /// Time between reads.
    pub period: Duration,
}

/// Poll `points` (all on one device) until `cancel` fires. `trust` is the
/// device's `x-device-trust` block; `creds` the gateway's mTLS material.
pub async fn run_device(
    points: Vec<Point>,
    client: paho_mqtt::AsyncClient,
    cancel: CancellationToken,
    trust: Option<DeviceTrust>,
    creds: Option<GatewayCredentials>,
) {
    let mut next = vec![Instant::now(); points.len()];
    // One Modbus session per host:port for this device's life; dropped (and
    // closed) when the poller stops.
    let mut sessions: HashMap<String, Channel> = HashMap::new();
    loop {
        let wake = next.iter().min().copied().unwrap_or_else(Instant::now);
        tokio::select! {
            () = cancel.cancelled() => break,
            () = sleep_until(wake) => {}
        }
        // Reason: a tick can spend minutes in retries against an
        // unreachable device; cancellation (shutdown, topology change)
        // must not wait for it.
        let tick = poll_due(
            &points,
            &mut next,
            &mut sessions,
            &client,
            trust.as_ref(),
            creds.as_ref(),
        );
        tokio::select! {
            () = cancel.cancelled() => break,
            () = tick => {}
        }
    }
}

/// Read and publish every reading due now, one at a time; readings that
/// share a Redfish resource share one fetch.
#[allow(clippy::too_many_arguments)]
async fn poll_due(
    points: &[Point],
    next: &mut [Instant],
    sessions: &mut HashMap<String, Channel>,
    client: &paho_mqtt::AsyncClient,
    trust: Option<&DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) {
    // Redfish resources fetched this tick, so readings that share one
    // (e.g. a GPU's power and its limit) cost one request between them.
    let mut fetched: HashMap<String, Result<Value, String>> = HashMap::new();
    for i in due(next, Instant::now()) {
        let p = &points[i];
        let read = match &p.binding {
            ProtocolBinding::Redfish(b) => {
                let key = format!("{}:{}{}", b.host, b.port, b.uri);
                if !fetched.contains_key(&key) {
                    let body = redfish::fetch_resource(b, trust, creds).await;
                    fetched.insert(key.clone(), body.map_err(|e| format!("{e:#}")));
                }
                match &fetched[&key] {
                    Ok(body) => redfish::extract(body, b),
                    Err(e) => Err(anyhow::anyhow!("{e}")),
                }
            }
            ProtocolBinding::ModbusTcp(b) => {
                let key = format!("{}:{}", b.host, b.port);
                if !sessions.contains_key(&key) {
                    match modbus::channel(b, trust, creds) {
                        Ok(channel) => {
                            sessions.insert(key.clone(), channel);
                        }
                        Err(e) => {
                            warn!(topic = %p.topic, error = %e, "modbus session failed");
                            next[i] = advance(next[i], p.period, Instant::now());
                            continue;
                        }
                    }
                }
                modbus::read_on(sessions.get_mut(&key).expect("just inserted"), b).await
            }
            other => read_value(other, trust, creds).await,
        };
        match read {
            Ok(value) => {
                if let Err(e) = publisher::publish_measurement(client, &p.topic, value).await {
                    warn!(topic = %p.topic, error = %e, "publish failed");
                }
            }
            Err(e) => warn!(topic = %p.topic, error = %e, "read failed; skipping tick"),
        }
        next[i] = advance(next[i], p.period, Instant::now());
    }
}

/// Indices of the readings due at `now`.
fn due(next: &[Instant], now: Instant) -> Vec<usize> {
    (0..next.len()).filter(|&i| next[i] <= now).collect()
}

/// When a reading read at `now` is next due. On schedule, one period after
/// the last due time; running late, one period from now (no catch-up burst).
fn advance(due_at: Instant, period: Duration, now: Instant) -> Instant {
    let on_schedule = due_at + period;
    if on_schedule <= now {
        now + period
    } else {
        on_schedule
    }
}

/// Single-point protocol dispatch. Add a `match` arm when a new
/// `ProtocolBinding` variant lands.
pub(crate) async fn read_value(
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
        // Synthetic channels are driven by `src/synthetic/`; the poller is
        // never handed one. Unreachable is a tripwire for that routing.
        ProtocolBinding::Synthetic(_) => {
            unreachable!("synthetic bindings are driven by the synthetic module, not the poller")
        }
        // Distribute is command-only; it never appears in x-protocol-source.
        ProtocolBinding::Distribute(_) => {
            unreachable!("distribute bindings are commands, never a measurement source")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_readings_whose_time_has_come_are_due() {
        let t0 = Instant::now();
        let next = [t0, t0 + Duration::from_secs(1), t0];
        assert_eq!(due(&next, t0), vec![0, 2]);
    }

    #[test]
    fn a_reading_on_schedule_is_next_due_one_period_later() {
        let t0 = Instant::now();
        let next = advance(t0, Duration::from_secs(2), t0 + Duration::from_millis(100));
        assert_eq!(next, t0 + Duration::from_secs(2));
    }

    #[test]
    fn a_late_reading_waits_a_full_period_instead_of_catching_up() {
        // Arrange — a slow device made this read 5 s late on a 1 s period
        let t0 = Instant::now();
        let now = t0 + Duration::from_secs(5);
        // Act + Assert
        assert_eq!(
            advance(t0, Duration::from_secs(1), now),
            now + Duration::from_secs(1)
        );
    }
}
