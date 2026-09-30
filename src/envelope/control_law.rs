//! BESS envelope actuation control law — clamp on violation, ramp back on
//! sustained recovery. Pure: no MQTT/Modbus, no async. Ticked once per
//! `active_power` sample (1 Hz) by the owning task; each tick reads the
//! module's real live state and returns the next setpoint to write, or
//! `None` if the output is unchanged this tick.
//!
//! The envelope is a transient override, not a source of intent: while a
//! limit isn't binding, output tracks `requested_setpoint` directly (the
//! last real operator/dispatcher command); while binding, output is
//! clamped to the limit; on sustained recovery, output ramps back toward
//! `requested_setpoint`, never toward some other value the operator never
//! asked for.
//!
//! Sign convention: positive active_power = discharge, negative = charge —
//! battery-referenced, same as bess_rack's active_power and the rest of the
//! dispatch path.
//!
//! With a POI meter, limits apply at the POI and the bounds come from
//! `poi_servo` (an integrator on measured POI headroom). Without one, the
//! BESS is treated as the only asset: discharge ≤ export_limit, charge ≥
//! −import_limit.

use crate::envelope::bounds;
pub use crate::envelope::inputs::{EnvelopeConfig, EnvelopeTick};
use std::time::Duration;

/// Which side of the envelope is currently binding, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// No limit binding — output tracks `requested_setpoint` directly.
    Normal,
    /// A limit is binding right now — output is clamped to it.
    Constrained,
    /// Limit cleared with sustained margin — output ramping back toward
    /// `requested_setpoint`.
    Ramping,
}

/// One module's envelope controller. Owns no I/O — the caller feeds it
/// live readings each tick and applies whatever it returns.
pub struct EnvelopeController {
    /// Which side of the envelope is currently binding, if any.
    mode: Mode,
    /// Consecutive time spent within the recovery margin while constrained;
    /// resets to zero the moment a sample falls back outside the margin.
    dwell_elapsed: Duration,
    /// The gateway's current belief of what the device holds — the ramp's
    /// starting point. Updated on every write this controller issues.
    current_output: f64,
    /// Configured ramp/hysteresis parameters for this controller.
    config: EnvelopeConfig,
    /// Last tick's limits, so the POI servo can tell a tightening from lag.
    prev_limits: (Option<f64>, Option<f64>),
    /// Last tick's `requested_setpoint`. A change means `handle_command`
    /// just wrote it straight to the device.
    last_requested: Option<f64>,
}

impl EnvelopeController {
    /// Build a controller starting in `Normal` mode, believing the device
    /// currently holds `initial_output`.
    #[must_use]
    pub fn new(config: EnvelopeConfig, initial_output: f64) -> Self {
        Self {
            mode: Mode::Normal,
            dwell_elapsed: Duration::ZERO,
            current_output: initial_output,
            config,
            prev_limits: (None, None),
            last_requested: None,
        }
    }

    /// The gateway's current belief of what the device holds — set by the
    /// most recent tick that actually changed it, whether or not the last
    /// call to `tick` itself returned `Some`.
    #[must_use]
    pub fn current_output(&self) -> f64 {
        self.current_output
    }

    /// Advance one tick. Returns `Some(new_setpoint)` if the gateway should
    /// write a new value this tick, `None` if the output is unchanged.
    pub fn tick(&mut self, input: EnvelopeTick) -> Option<f64> {
        // Reason: an operator command is written to the device directly,
        // outside this controller. Knowing that, the device now holds it —
        // more exact than any reading, which may still be catching up.
        if self
            .last_requested
            .is_some_and(|r| r != input.requested_setpoint)
        {
            self.current_output = input.requested_setpoint;
        }
        self.last_requested = Some(input.requested_setpoint);
        let bounds::Bounds {
            ceiling,
            floor,
            headroom_export,
            headroom_import,
        } = bounds::for_tick(self.current_output, self.prev_limits, &input);
        // Never past the module's own rating, whichever limit asks for it.
        let ceiling = ceiling.min(input.power_max);
        let floor = floor.max(input.power_min).min(ceiling);
        self.prev_limits = (input.import_limit, input.export_limit);

        if headroom_export <= 0.0 || headroom_import <= 0.0 {
            // Tightening, or a fresh violation — instant clamp, no ramp,
            // regardless of prior mode. A brief undershoot is safe; an
            // overshoot past the limit is the failure mode this exists to
            // prevent. Headroom reaching exactly zero counts (not just
            // going negative) — the limit is already fully used.
            self.mode = Mode::Constrained;
            self.dwell_elapsed = Duration::ZERO;
            return self.write(input.requested_setpoint.clamp(floor, ceiling));
        }

        if self.mode == Mode::Constrained {
            let margin_export = self.config.hysteresis_margin * input.power_max;
            let margin_import = self.config.hysteresis_margin * input.power_min.abs();
            if headroom_export >= margin_export && headroom_import >= margin_import {
                self.dwell_elapsed += input.dt;
                if self.dwell_elapsed >= self.config.hysteresis_dwell {
                    self.mode = Mode::Ramping;
                }
            } else {
                self.dwell_elapsed = Duration::ZERO;
            }
            if self.mode == Mode::Constrained {
                // Still holding the clamped value; dwell not yet satisfied.
                return None;
            }
            // Dwell just closed this tick — fall through and start ramping
            // now rather than waiting a whole extra tick to react.
        }

        match self.mode {
            // Defensively clamped even though headroom (based on the real,
            // possibly-stale active_power reading) hasn't reached zero yet
            // — never write past a known limit regardless of mode.
            Mode::Normal => self.write(input.requested_setpoint.clamp(floor, ceiling)),
            Mode::Ramping => {
                let target = input.requested_setpoint.clamp(floor, ceiling);
                let delta = target - self.current_output;
                // Rated power is direction-dependent: ramping toward
                // discharge (positive) draws on power_max, toward charge
                // (negative) on power_min's magnitude.
                let rated = if delta >= 0.0 {
                    input.power_max
                } else {
                    input.power_min.abs()
                };
                let max_step = self.config.ramp_rate_per_sec * rated * input.dt.as_secs_f64();
                let next = if delta.abs() <= max_step {
                    self.mode = Mode::Normal;
                    target
                } else {
                    self.current_output + max_step * delta.signum()
                };
                self.write(next)
            }
            Mode::Constrained => unreachable!("handled above"),
        }
    }

    /// Update `current_output` and return `Some(value)` if it actually
    /// changed this tick.
    fn write(&mut self, value: f64) -> Option<f64> {
        if (value - self.current_output).abs() < f64::EPSILON {
            return None;
        }
        self.current_output = value;
        Some(value)
    }
}

impl EnvelopeController {
    /// Pull the controller's output back to what distribution could
    /// actually deliver (eligible children's capacity).
    ///
    /// Reason: anti-windup. With children excluded (e.g. parked at their
    /// reserve floor) the POI keeps showing headroom or a violation that
    /// more command can't fix, and the servo would keep integrating. When
    /// they came back, the wound-up command went out in one tick.
    pub fn sync_to_delivered(&mut self, delivered: f64) {
        self.current_output = delivered;
    }
}
