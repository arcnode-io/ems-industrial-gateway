//! Re-fetches the locked set on every `system/loto_changed` beacon. A failed
//! re-fetch keeps the last known set: unsure is treated as still locked.

use super::LockedDevices;
use crate::http::client::fetch_loto;
use std::collections::HashSet;
use tokio::sync::watch;
use tracing::{info, warn};

/// Spawn the re-fetch loop; it ends when the beacon sender drops.
pub fn spawn(
    device_api_url: String,
    locked: LockedDevices,
    mut beacons: watch::Receiver<u64>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while beacons.changed().await.is_ok() {
            match fetch_loto(&device_api_url).await {
                Ok(fresh) => {
                    apply(&locked, fresh).await;
                }
                Err(e) => warn!(
                    error = format!("{e:#}"),
                    "/loto re-fetch failed; keeping the last locked set"
                ),
            }
        }
    })
}

/// Swap in `fresh`; returns whether it changed anything. Reason: device-api
/// beacons on every broker reconnect too, so most beacons change nothing.
pub async fn apply(locked: &LockedDevices, fresh: HashSet<String>) -> bool {
    let mut current = locked.write().await;
    if *current == fresh {
        return false;
    }
    let mut ids: Vec<&String> = fresh.iter().collect();
    ids.sort();
    info!(locked = ?ids, "lockouts changed");
    *current = fresh;
    true
}

#[cfg(test)]
#[path = "refresher_test.rs"]
mod tests;
