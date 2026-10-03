//! What the envelope controller is configured with and fed each tick.

use std::time::Duration;

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

/// One tick's live inputs.
#[derive(Debug, Clone, Copy)]
pub struct EnvelopeTick {
    /// Live `import_limit` (a positive magnitude) — caps charging. `None`
    /// until the upstream signal has published one.
    pub import_limit: Option<f64>,
    /// Live `export_limit` (a positive magnitude) — caps discharging.
    pub export_limit: Option<f64>,
    /// The module's real, current `active_power` reading.
    pub active_power: f64,
    /// The last real operator/dispatcher setpoint request — the ramp target.
    pub requested_setpoint: f64,
    /// The POI meter's `active_power` (+ import); `None` when the site has
    /// no POI meter, which is the battery-only law.
    pub poi_active_power: Option<f64>,
    /// Whether `poi_active_power` is a reading no earlier tick has used. A
    /// reused reading must not be integrated again.
    pub poi_fresh: bool,
    /// Hold movement toward the limit (a share handoff is in flight and its
    /// deliberate under-delivery would read as headroom). Cuts still apply.
    pub hold_approach: bool,
    /// Module's own static nameplate lower bound (max charge; negative).
    /// Ramp rate and hysteresis margin on the charge/import side are
    /// fractions of its magnitude.
    pub power_min: f64,
    /// Module's own static nameplate upper bound (max discharge; positive).
    /// Ramp rate and hysteresis margin on the discharge/export side are
    /// fractions of this.
    pub power_max: f64,
    /// Time since the previous tick.
    pub dt: Duration,
}
