//! What site distribution remembers about the current curtailment event.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Event memory shared across site-distribution task respawns.
pub type SharedEventMemory = Arc<Mutex<EventMemory>>;

/// Build an empty shared event memory.
#[must_use]
pub fn new_event_memory() -> SharedEventMemory {
    Arc::new(Mutex::new(EventMemory::default()))
}

/// Operator setpoints from before the event, and what the event dispatched.
#[derive(Default)]
pub struct EventMemory {
    /// Operator setpoint per module, captured before the event's first
    /// dispatch. `None` = no event in progress.
    snapshot: Option<HashMap<String, f64>>,
    /// Last share the event dispatched per module.
    dispatched: HashMap<String, f64>,
}

/// Two setpoints within this are the same command (they round-trip through
/// a JSON payload and `last_requested`).
const SAME_SETPOINT_W: f64 = 1e-6;

impl EventMemory {
    /// Capture operator setpoints at the start of an event. Only the first
    /// call per event snapshots: after the event dispatches, the operator map
    /// holds the event's own values, and re-snapshotting would "restore" the
    /// curtailment itself.
    pub fn begin(&mut self, operator: impl FnOnce() -> HashMap<String, f64>) {
        if self.snapshot.is_none() {
            self.snapshot = Some(operator());
        }
    }

    /// Shares that differ from what the event last dispatched.
    #[must_use]
    pub fn changed(&self, shares: Vec<(String, f64)>) -> Vec<(String, f64)> {
        shares
            .into_iter()
            .filter(|(id, share)| {
                self.dispatched
                    .get(id)
                    .is_none_or(|prev| (share - prev).abs() >= SAME_SETPOINT_W)
            })
            .collect()
    }

    /// Record a share the event dispatched to a module.
    pub fn record(&mut self, module_id: &str, share: f64) {
        self.dispatched.insert(module_id.to_string(), share);
    }

    /// Restore commands for an ended event: each module the event dispatched
    /// goes back to its pre-event operator setpoint (0 if none), unless its
    /// current setpoint (`current`) is no longer the event's, meaning an
    /// operator commanded it mid-event and that command stands.
    #[must_use]
    pub fn end(&self, current: &HashMap<String, f64>) -> Vec<(String, f64)> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        self.dispatched
            .iter()
            .filter(|(id, sent)| {
                current
                    .get(*id)
                    .is_some_and(|now| (now - *sent).abs() < SAME_SETPOINT_W)
            })
            .map(|(id, _)| (id.clone(), snapshot.get(id).copied().unwrap_or(0.0)))
            .collect()
    }

    /// Forget the event, once every restore command has been published.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
#[path = "event_memory_test.rs"]
mod tests;
