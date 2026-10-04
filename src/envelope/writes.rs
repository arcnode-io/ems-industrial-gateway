//! An envelope task's memory of its own child writes: what each child last
//! got, whether growth was held back last tick, and the POI gate that must
//! hear about a handoff.

use crate::dispatch;
use crate::envelope::poi_gate::PoiGate;
use std::collections::HashMap;
use std::time::Instant;

/// Per-task write state, kept across ticks.
#[derive(Debug, Default)]
pub struct WriteState {
    /// Last share actually landed per child: what a change is measured
    /// against and what an unchanged child is re-asserted at.
    pub last_written: HashMap<String, f64>,
    /// Whether last tick held back a growing share.
    deferred_growth: bool,
    /// Freshness and handoff gate for the POI servo.
    pub gate: PoiGate,
}

/// This tick's writes, and whether they complete a held-back growth.
pub struct Plan {
    /// Child writes, shrinks first.
    pub writes: Vec<(String, f64)>,
    /// True when these writes include growth held back last tick.
    completes_handoff: bool,
}

impl WriteState {
    /// Pick this tick's writes from the freshly computed `shares`: changes
    /// in handoff order, then every other child re-asserted at its last
    /// landed share. `None` only before anything is known to write.
    ///
    /// Reason: a rack that reboots or is overridden locally drops our
    /// setpoint while its share hasn't changed. Writing only changes left one
    /// free-running at its boot default until the gateway restarted.
    pub fn plan(&mut self, shares: Vec<(String, f64)>) -> Option<Plan> {
        let ids: Vec<String> = shares.iter().map(|(id, _)| id.clone()).collect();
        let changed: Vec<(String, f64)> = shares
            .into_iter()
            .filter(|(id, share)| {
                self.last_written
                    .get(id)
                    .is_none_or(|prev| (share - prev).abs() >= f64::EPSILON)
            })
            .collect();
        let held = self.deferred_growth;
        let mut writes = Vec::new();
        if !changed.is_empty() {
            let ordered = dispatch::reductions_first(changed, &self.last_written);
            let (batch, deferred) = dispatch::handoff_batch(ordered, &self.last_written, held);
            self.deferred_growth = deferred;
            if deferred {
                self.gate.growth_deferred();
            }
            writes = batch;
        }
        let reasserts: Vec<(String, f64)> = ids
            .into_iter()
            .filter(|id| writes.iter().all(|(w, _)| w != id))
            .filter_map(|id| self.last_written.get(&id).map(|&last| (id, last)))
            .collect();
        writes.extend(reasserts);
        if writes.is_empty() {
            return None;
        }
        Some(Plan {
            writes,
            completes_handoff: held && !self.deferred_growth,
        })
    }

    /// Record that `plan`'s writes landed.
    pub fn landed(&mut self, plan: Plan) {
        if plan.completes_handoff {
            self.gate.growth_written(Instant::now());
        }
        self.last_written.extend(plan.writes);
    }
}

#[cfg(test)]
#[path = "writes_test.rs"]
mod tests;
