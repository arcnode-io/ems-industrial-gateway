//! Lockout/tagout: the devices a person has locked out for work. The gateway
//! never writes to one. device-api owns the locks and expands each to its
//! whole DTM subtree; `GET /loto` is the truth, `system/loto_changed` only a
//! nudge to re-fetch it.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;

pub mod refresher;

/// Device ids the gateway refuses writes to, shared with every write path.
pub type LockedDevices = Arc<RwLock<HashSet<String>>>;

/// `GET /loto`, narrowed to what the gateway enforces.
#[derive(Debug, Deserialize)]
struct LotoResponse {
    /// Every locked device and its subtree, expanded by device-api.
    locked_devices: Vec<String>,
}

/// The locked set from a `GET /loto` body.
pub fn parse(body: &str) -> Result<HashSet<String>> {
    let r: LotoResponse = serde_json::from_str(body).context("parse /loto")?;
    Ok(r.locked_devices.into_iter().collect())
}

#[cfg(test)]
#[path = "loto_test.rs"]
mod tests;
