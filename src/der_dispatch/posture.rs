//! What der_dispatch's retained channels say site distribution should do
//! this tick.
//!
//! Reason: `event_active` says an event is in force, not that it commands
//! power. An energize-only control (2030.5 `opModEnergize`, no target, no
//! limits) is active with `target_active_power` 0 and
//! `target_setpoint_present` false; dispatching that 0 would stop the
//! battery for a control that said nothing about power.

use crate::synthetic::{InputCache, as_number};

/// Site distribution's move for one tick.
#[derive(Debug, PartialEq)]
pub enum Posture {
    /// An input hasn't landed yet: change nothing.
    Hold,
    /// No power setpoint in force: modules go back to their operators'.
    Release,
    /// Split this site target (W) across the modules.
    Dispatch(f64),
}

/// The der_dispatch topics a posture is read from (`{site}` substituted).
pub struct PostureTopics {
    /// `.../der_dispatch/measurements/event_active/none`.
    pub event_active: String,
    /// `.../der_dispatch/measurements/target_setpoint_present/none`.
    pub target_present: String,
    /// `.../der_dispatch/measurements/target_active_power/watts`.
    pub target: String,
}

/// Read this tick's posture from the cache.
pub fn posture(topics: &PostureTopics, cache: &InputCache) -> Posture {
    let read = |topic: &str| cache.get(topic).and_then(|e| as_number(&e.0));
    match read(&topics.event_active) {
        None => return Posture::Hold,
        Some(active) if active < 0.5 => return Posture::Release,
        Some(_) => {}
    }
    match (read(&topics.target_present), read(&topics.target)) {
        (Some(present), _) if present < 0.5 => Posture::Release,
        (Some(_), Some(target)) => Posture::Dispatch(target),
        _ => Posture::Hold,
    }
}

#[cfg(test)]
#[path = "posture_test.rs"]
mod tests;
