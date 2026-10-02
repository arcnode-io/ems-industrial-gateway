//! Compute shed: the second lever after storage. Drives a fleet power-cap
//! percentage (see `dispatch::power_cap`) so the POI stays inside the
//! envelope when the battery can't hold it alone — power-limited, at its
//! reserve floor, absent, or deliberately not dispatched.
//!
//! Lever order is storage first, compute second: compute only sheds once
//! import has stayed over the limit for the full hysteresis dwell. Invariant:
//! that dwell must be longer than the BESS envelope servo takes to settle,
//! so compute only ever covers what storage evidently couldn't.
//!
//! Both envelope bounds hold: exporting past the export limit means shedding
//! overshot (storage plus shed load), so caps come back at once, no dwell.

use crate::envelope::inputs::EnvelopeConfig;
use std::time::Duration;

/// One tick's inputs.
#[derive(Debug, Clone, Copy)]
pub struct ShedTick {
    /// POI `active_power`, + import.
    pub poi_active_power: f64,
    /// `import_limit` magnitude.
    pub import_limit: f64,
    /// `export_limit` magnitude.
    pub export_limit: f64,
    /// The operator's fleet cap (100 if none): the most shedding restores to.
    pub requested_percent: f64,
    /// Sum of every child's max cap.
    pub fleet_max_w: f64,
    /// Sum of every child's min cap.
    pub fleet_min_w: f64,
    /// Time since the last tick.
    pub dt: Duration,
}

/// Fleet cap state across ticks.
pub struct ShedController {
    config: EnvelopeConfig,
    /// Current fleet cap, whole percent; `None` before the first tick.
    percent: Option<f64>,
    /// How long import has stayed over the limit.
    over_for: Duration,
    /// How long import has had recovery headroom while shed.
    clear_for: Duration,
}

impl ShedController {
    /// A controller at no shed.
    pub fn new(config: EnvelopeConfig) -> Self {
        Self {
            config,
            percent: None,
            over_for: Duration::ZERO,
            clear_for: Duration::ZERO,
        }
    }

    /// The fleet cap percentage to write, or `None` if unchanged.
    pub fn tick(&mut self, t: &ShedTick) -> Option<f64> {
        // Never above the operator's cap; their own command already wrote it.
        let current = self
            .percent
            .unwrap_or(t.requested_percent)
            .min(t.requested_percent);
        let watts_to_percent = 100.0 / t.fleet_max_w;
        let step = self.config.ramp_rate_per_sec * 100.0 * t.dt.as_secs_f64();
        let over = t.poi_active_power - t.import_limit;
        let exported = -t.poi_active_power - t.export_limit;
        let headroom =
            t.import_limit - t.poi_active_power - self.config.hysteresis_margin * t.fleet_max_w;
        let target = if exported > 0.0 {
            self.reset();
            current + (exported * watts_to_percent).min(step)
        } else if over > 0.0 {
            self.clear_for = Duration::ZERO;
            self.over_for += t.dt;
            if self.over_for < self.config.hysteresis_dwell {
                return None;
            }
            current - (over * watts_to_percent).min(step)
        } else if headroom > 0.0 && current < t.requested_percent {
            self.over_for = Duration::ZERO;
            self.clear_for += t.dt;
            if self.clear_for < self.config.hysteresis_dwell {
                return None;
            }
            current + (headroom * watts_to_percent).min(step)
        } else {
            self.reset();
            current
        };
        let floor = t.fleet_min_w * watts_to_percent;
        let next = target.clamp(floor, t.requested_percent).round();
        self.percent = Some(next);
        (next != current.round()).then_some(next)
    }

    /// Clear both dwell timers.
    fn reset(&mut self) {
        self.over_for = Duration::ZERO;
        self.clear_for = Duration::ZERO;
    }
}

#[cfg(test)]
#[path = "shed_test.rs"]
mod tests;
