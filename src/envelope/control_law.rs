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

/// Configured control-law parameters — engineering-judgment defaults, not
/// sourced from a verified standard (see power-engineer's envelope-control-
/// law handoff, 2026-09-18). Configured per module, not hardcoded, so they
/// can be corrected once a real IEEE 1547 / interconnection number is in
/// hand without a code change.
#[derive(Debug, Clone, Copy)]
pub struct EnvelopeConfig {
    /// Ramp rate on recovery, as a fraction of rated power per second
    /// (e.g. `0.10` = 10%/sec).
    pub ramp_rate_per_sec: f64,
    /// Required headroom margin to close a constrained event, as a
    /// fraction of rated power (e.g. `0.05` = 5%).
    pub hysteresis_margin: f64,
    /// How long that margin must hold continuously before closing the event.
    pub hysteresis_dwell: Duration,
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
}

/// One tick's live inputs.
#[derive(Debug, Clone, Copy)]
pub struct EnvelopeTick {
    /// Live `import_limit`, if the upstream signal has published one yet.
    pub import_limit: Option<f64>,
    /// Live `export_limit` (a positive magnitude), if published.
    pub export_limit: Option<f64>,
    /// The module's real, current `active_power` reading.
    pub active_power: f64,
    /// The last real operator/dispatcher setpoint request — the ramp target.
    pub requested_setpoint: f64,
    /// Module's own static nameplate lower bound (e.g. max charge; negative).
    /// Ramp rate and hysteresis margin on the export side are fractions of
    /// its magnitude.
    pub power_min: f64,
    /// Module's own static nameplate upper bound (e.g. max discharge;
    /// positive). Ramp rate and hysteresis margin on the import side are
    /// fractions of this.
    pub power_max: f64,
    /// Time since the previous tick.
    pub dt: Duration,
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
        }
    }

    /// Advance one tick. Returns `Some(new_setpoint)` if the gateway should
    /// write a new value this tick, `None` if the output is unchanged.
    pub fn tick(&mut self, input: EnvelopeTick) -> Option<f64> {
        let ceiling = input.import_limit.unwrap_or(f64::INFINITY);
        let floor = input.export_limit.map(|e| -e).unwrap_or(f64::NEG_INFINITY);
        let headroom_import = ceiling - input.active_power;
        let headroom_export = input.active_power - floor;

        if headroom_import <= 0.0 || headroom_export <= 0.0 {
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
            let margin_import = self.config.hysteresis_margin * input.power_max;
            let margin_export = self.config.hysteresis_margin * input.power_min.abs();
            if headroom_import >= margin_import && headroom_export >= margin_export {
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
                // import (positive) draws on power_max, toward export
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
