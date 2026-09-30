//! Gates the POI servo on how old its reading is relative to its own writes:
//! integrate each reading once, and hold the approach while a share handoff
//! is in flight (`dispatch::handoff_batch`).

use std::time::Instant;

/// Fresh readings after the growth write before the hold lifts.
///
/// Reason: a reading that merely arrives after the write can still describe
/// the moment before it; the rack takes up to a tick to respond and the
/// meter samples it on its own tick. The second fresh reading can't.
const READINGS_AFTER_GROWTH: u8 = 2;

/// Where a share handoff stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handoff {
    /// No handoff in flight.
    Idle,
    /// Growth held back this tick; not yet written.
    Deferred,
    /// Growth written; counting fresh readings since.
    GrowthWritten {
        /// When the growth write landed.
        at: Instant,
        /// Fresh readings received after `at` so far.
        seen: u8,
    },
}

/// Per-task freshness and handoff state for the POI servo.
#[derive(Debug)]
pub struct PoiGate {
    /// Receipt time of the POI reading the last tick used.
    last_used: Option<Instant>,
    /// Handoff progress.
    handoff: Handoff,
}

impl PoiGate {
    /// No reading used yet, no handoff.
    #[must_use]
    pub fn new() -> Self {
        Self {
            last_used: None,
            handoff: Handoff::Idle,
        }
    }

    /// Take this tick's POI reading (by its receipt time). Returns
    /// `(fresh, hold_approach)` for the controller tick.
    pub fn take(&mut self, received_at: Instant) -> (bool, bool) {
        let fresh = self.last_used != Some(received_at);
        self.last_used = Some(received_at);
        if let Handoff::GrowthWritten { at, seen } = self.handoff
            && fresh
            && received_at > at
        {
            let seen = seen + 1;
            self.handoff = if seen >= READINGS_AFTER_GROWTH {
                Handoff::Idle
            } else {
                Handoff::GrowthWritten { at, seen }
            };
        }
        (fresh, self.handoff != Handoff::Idle)
    }

    /// Growth was held back this tick.
    pub fn growth_deferred(&mut self) {
        self.handoff = Handoff::Deferred;
    }

    /// The held-back growth was written at `at`.
    pub fn growth_written(&mut self, at: Instant) {
        if self.handoff == Handoff::Deferred {
            self.handoff = Handoff::GrowthWritten { at, seen: 0 };
        }
    }
}

impl Default for PoiGate {
    fn default() -> Self {
        Self::new()
    }
}
