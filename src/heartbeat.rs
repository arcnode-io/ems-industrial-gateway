//! Template-declared heartbeats (`x-heartbeat-source`): a counter written
//! once a period so the device knows the EMS is alive. Like every
//! watchdog feed in the gateway, it only beats while the EMS can steer.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::ModbusTcpBinding;
use crate::config::GatewayCredentials;
use crate::loto::LockedDevices;
use crate::modbus::client::write_setpoint;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Whether a watchdog feed should beat: the broker link is up (the EMS can
/// still steer) and `device_id` isn't locked out.
pub async fn steerable(
    client: &paho_mqtt::AsyncClient,
    locked: &LockedDevices,
    device_id: &str,
) -> bool {
    client.is_connected() && !locked.read().await.contains(device_id)
}

/// One counter heartbeat, ready to run.
pub struct Counter {
    /// The device it keeps alive.
    pub device_id: String,
    /// Where the counter is written.
    pub binding: ModbusTcpBinding,
    /// The counter wraps to 0 here.
    pub modulo: u16,
    /// Time between writes.
    pub period: Duration,
}

/// Write `c`'s counter every period until `cancel` fires, counting on from
/// each successful write.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    c: Counter,
    client: paho_mqtt::AsyncClient,
    locked: LockedDevices,
    trust: Option<DeviceTrust>,
    creds: Option<GatewayCredentials>,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!(device_id = %c.device_id, period = ?c.period, "heartbeat started");
        let mut tick = interval(c.period);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut count: u16 = 0;
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = tick.tick() => {}
            }
            if !steerable(&client, &locked, &c.device_id).await {
                continue;
            }
            let value = f64::from(count);
            match write_setpoint(&c.binding, value, trust.as_ref(), creds.as_ref()).await {
                Ok(()) => count = (count + 1) % c.modulo,
                Err(e) => {
                    warn!(device_id = %c.device_id, error = format!("{e:#}"), "heartbeat write failed")
                }
            }
        }
    })
}
