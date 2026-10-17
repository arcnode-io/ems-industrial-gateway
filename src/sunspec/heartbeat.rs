//! The CmdBits heartbeat. EPC's PCS drops out unless CmdBits is rewritten
//! at least once a second in every state; that's a watchdog, so the
//! gateway only feeds it while the EMS can still steer: broker link up and
//! the device not locked out. Otherwise the beat stops, the asserted word
//! is forgotten, and the PCS trips on its own watchdog.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::{CommandSource, ProtocolBinding, SunspecBinding};
use crate::config::GatewayCredentials;
use crate::heartbeat::steerable;
use crate::loto::LockedDevices;
use crate::sunspec::bits::{WordKey, slot, word_key};
use crate::sunspec::write::{read_word, write_word};
use std::collections::HashMap;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Time between beats: half EPC's 1 s deadline, so one late beat is no trip.
pub const BEAT: Duration = Duration::from_millis(500);

/// One word to keep alive: where it is and the bits the gateway owns in it.
pub struct Beat {
    /// The device the word belongs to, for the lockout check.
    pub device_id: String,
    /// The word's point, with no `bit`.
    pub point: SunspecBinding,
    /// Bits some command binds; they start at 0.
    pub owned: u32,
}

/// One `Beat` per bitfield point a device's commands name a bit of.
pub fn beats(commands: &HashMap<String, HashMap<String, CommandSource>>) -> Vec<Beat> {
    let mut found: HashMap<WordKey, Beat> = HashMap::new();
    for (device_id, channels) in commands {
        for source in channels.values() {
            let ProtocolBinding::Sunspec(b) = &source.binding else {
                continue;
            };
            let Some(bit) = b.bit else { continue };
            let beat = found.entry(word_key(b)).or_insert_with(|| Beat {
                device_id: device_id.clone(),
                point: SunspecBinding {
                    bit: None,
                    ..clone(b)
                },
                owned: 0,
            });
            beat.owned |= 1 << bit;
        }
    }
    found.into_values().collect()
}

/// Feed `beat` every `BEAT` until `cancel` fires.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    beat: Beat,
    client: paho_mqtt::AsyncClient,
    locked: LockedDevices,
    trust: Option<DeviceTrust>,
    creds: Option<GatewayCredentials>,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        info!(device_id = %beat.device_id, point = %beat.point.point, "heartbeat started");
        let slot = slot(&word_key(&beat.point));
        let mut tick = interval(BEAT);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = tick.tick() => {}
            }
            let mut word = slot.lock().await;
            if !steerable(&client, &locked, &beat.device_id).await {
                *word = None;
                continue;
            }
            let (trust, creds) = (trust.as_ref(), creds.as_ref());
            if word.is_none() {
                match read_word(&beat.point, trust, creds).await {
                    Ok(read) => *word = Some(read & !beat.owned),
                    Err(e) => {
                        warn!(device_id = %beat.device_id, error = format!("{e:#}"), "heartbeat seed failed");
                        continue;
                    }
                }
            }
            let asserted = word.expect("seeded above");
            if let Err(e) = write_word(&beat.point, asserted, trust, creds).await {
                warn!(device_id = %beat.device_id, error = format!("{e:#}"), "heartbeat write failed");
            }
        }
    })
}

/// Field-wise copy; bindings stay `!Clone` (see `app::clone_binding`).
fn clone(b: &SunspecBinding) -> SunspecBinding {
    SunspecBinding {
        host: b.host.clone(),
        port: b.port,
        unit_id: b.unit_id.clone(),
        model_id: b.model_id,
        point: b.point.clone(),
        bit: b.bit,
    }
}
