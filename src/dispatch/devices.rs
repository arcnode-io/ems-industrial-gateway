//! The live device maps every write path resolves a binding from, and the
//! devices it must not write to.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::ProtocolBinding;
use crate::loto::LockedDevices;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Shared handles, cloned into every task that writes south.
#[derive(Clone)]
pub struct Devices {
    /// Device → channel → binding, refreshed on every spec re-fetch.
    pub channels: Arc<RwLock<HashMap<String, HashMap<String, ProtocolBinding>>>>,
    /// Device → trust material, refreshed alongside `channels`.
    pub trust: Arc<RwLock<HashMap<String, DeviceTrust>>>,
    /// Locked-out devices, refreshed on every `system/loto_changed`.
    pub locked: LockedDevices,
}
