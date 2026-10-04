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

/// A tick-on-tick drop in import smaller than this fraction of the fleet's
/// max counts as flat: meter noise, not storage still ramping.
const SHRINK_DEADBAND: f64 = 0.001;

/// The longest POI meter lag the envelope's servos are tuned for (see
/// `poi_servo`).
const METER_LAG_SECS: f64 = 3.0;

/// Cap resolution: 0.1% is 1 W on a 1000 W GPU limit, the device's own.
const STEP_PERCENT: f64 = 0.1;

/// `target` on the `STEP_PERCENT` grid, rounded toward `current` when
/// cutting. Reason: a cut rounded up past the gap exports the difference
/// (a whole 1% step on a 0.83% gap exported 1.3 kW on the demo). A raise
/// rounds up, which only errs toward import.
fn quantize(current: f64, target: f64) -> f64 {
    // ceil: toward current for a cut (negative), up for a raise
    let steps = ((target - current) / STEP_PERCENT).ceil();
    ((current + steps * STEP_PERCENT) * 10.0).round() / 10.0
}

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
    /// Discharge the site's storage could add right now (capability above
    /// its effective floor minus what it delivers); 0 when none.
    pub storage_spare_w: f64,
    /// Time since the last tick.
    pub dt: Duration,
}

/// Fleet cap state across ticks.
pub struct ShedController {
    /// Dwell, margin and ramp, shared with the BESS envelope guard.
    config: EnvelopeConfig,
    /// Fleet cap the devices hold, in `STEP_PERCENT` steps; `None` before
    /// any write.
    percent: Option<f64>,
    /// How long import has stayed over the limit.
    over_for: Duration,
    /// How long import has had recovery headroom while shed.
    clear_for: Duration,
    /// Last tick's import over the limit, to tell storage still closing the
    /// gap from storage done.
    last_over: Option<f64>,
}

impl ShedController {
    /// A controller at no shed.
    pub fn new(config: EnvelopeConfig) -> Self {
        Self {
            config,
            percent: None,
            over_for: Duration::ZERO,
            clear_for: Duration::ZERO,
            last_over: None,
        }
    }

    /// The fleet cap percentage to write, or `None` if unchanged. Takes
    /// effect only once `confirm`ed: a write that failed leaves the devices
    /// where they were, so the next tick proposes from there again.
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
        let margin_w = self.config.hysteresis_margin * t.fleet_max_w;
        let headroom = t.import_limit - t.poi_active_power - margin_w;
        // Reason: handing load back to storage puts the POI a little over the
        // limit until the battery's servo catches up (each raise lands on the
        // meter first). Within the margin, with spare storage, that's the
        // servo tracking, not import storage can't cover.
        let tracking = over <= margin_w && t.storage_spare_w > 0.0;
        let target = if exported > 0.0 {
            self.reset();
            current + (exported * watts_to_percent).min(step)
        } else if over > 0.0 && !tracking {
            self.clear_for = Duration::ZERO;
            // Reason: lever order. While import is still falling, storage is
            // still closing the gap, so the dwell restarts; compute only
            // sheds what storage evidently can't cover. Once shedding has
            // begun its own cuts shrink import, so this only gates the start.
            let storage_closing = self
                .last_over
                .is_some_and(|prev| prev - over > SHRINK_DEADBAND * t.fleet_max_w);
            self.last_over = Some(over);
            if storage_closing && self.over_for < self.config.hysteresis_dwell {
                self.over_for = Duration::ZERO;
                return None;
            }
            self.over_for += t.dt;
            if self.over_for < self.config.hysteresis_dwell {
                return None;
            }
            current - (over * watts_to_percent).min(step)
        } else if let Some(room) = self.room_to_restore(t, headroom)
            && current < t.requested_percent
        {
            self.over_for = Duration::ZERO;
            self.clear_for += t.dt;
            if self.clear_for < self.config.hysteresis_dwell {
                return None;
            }
            current + (room * watts_to_percent).min(step)
        } else {
            self.reset();
            current
        };
        let floor = t.fleet_min_w * watts_to_percent;
        let next = quantize(current, target).clamp(floor, t.requested_percent);
        ((next - current).abs() >= STEP_PERCENT / 2.0).then_some(next)
    }

    /// Record that `percent` reached every device.
    pub fn confirm(&mut self, percent: f64) {
        self.percent = Some(percent);
    }

    /// Watts compute may take back this tick, or `None` for none: the import
    /// headroom once the limit lifts, else what storage could pick up while
    /// the POI tracks the limit (within the margin).
    ///
    /// Reason: lever order runs both ways. Compute is the last resort, so
    /// spare storage takes its load back first. That hand-back is limited to
    /// what the battery's servo can follow through the meter's lag (the
    /// margin over `METER_LAG_SECS` per second), so import stays within the
    /// margin while the battery catches up.
    fn room_to_restore(&self, t: &ShedTick, headroom: f64) -> Option<f64> {
        if headroom > 0.0 {
            return Some(headroom);
        }
        let margin_w = self.config.hysteresis_margin * t.fleet_max_w;
        let follow_w = margin_w / METER_LAG_SECS * t.dt.as_secs_f64();
        (t.storage_spare_w > 0.0).then(|| t.storage_spare_w.min(follow_w))
    }

    /// Clear both dwell timers and the import trend.
    fn reset(&mut self) {
        self.over_for = Duration::ZERO;
        self.clear_for = Duration::ZERO;
        self.last_over = None;
    }
}

#[cfg(test)]
#[path = "shed_test.rs"]
mod tests;
