//! POI-referenced envelope bounds: servo the battery on the measured POI,
//! never on a computed site load.
//!
//! Reason: the POI meter reads the battery's own steps 1–3 s late. Any law
//! that computes load as `P_poi + P_bess` and clamps to it counts each step
//! twice and feeds on its own lag. That rang into export on the demo. Here
//! the bounds are the commanded output nudged by a fraction of the measured
//! POI headroom per second, which is an integrator: slow enough, it can't
//! overshoot through the lag. It steps from what was commanded, never from
//! the battery's reading: a summed reading lags a tick or two, and pairing a
//! stale battery total with a fresh POI re-creates the double count.
//!
//! Sign: `p_poi` + = import. Export headroom `p_poi + export_limit` (room
//! to discharge more), import headroom `import_limit − p_poi` (room to
//! charge more).

use crate::envelope::bounds::Bounds;
use std::time::Duration;

/// Gain while approaching a limit, per second of headroom.
///
/// Reason: an integrator behind an n-tick meter lag overshoots once gain ×
/// lag gets large. At 1 Hz, 0.1/s gives zero overshoot up to a 3 s lag and
/// 1.3% at 4 s, settling in ~30 s. Overshoot here is export (or import) past
/// the limit, so it's tuned for lag, not speed.
pub const APPROACH_GAIN_PER_SEC: f64 = 0.1;

/// Gain while correcting a measured violation, per second of excess.
///
/// Reason: an integrator behind n ticks of lag is stable while gain <
/// 2·sin(π/(2(2n+1))): 0.62 / 0.45 / 0.35 for n = 2 / 3 / 4 s. 0.3 stays
/// stable to 4 s and, after a 2 s-lagged load drop, clears export in ~6 s
/// while over-correcting ~135 kW of a 520 kW drop.
/// Over-correcting only backs away from the limit (safe side), and it's
/// capped by the opposite side's headroom so a tight band can't ring.
pub const VIOLATION_GAIN_PER_SEC: f64 = 0.3;

/// A limit, with the value it had on the previous tick.
#[derive(Debug, Clone, Copy)]
pub struct Limit {
    /// This tick's value; `None` = unconstrained.
    pub now: Option<f64>,
    /// Previous tick's value; `None` = unconstrained (or first tick).
    pub prev: Option<f64>,
}

/// Bounds for the next output, from the commanded output `u` and a POI
/// reading.
#[must_use]
pub fn bounds(u: f64, p_poi: f64, import: Limit, export: Limit, dt: Duration) -> Bounds {
    let headroom_export = headroom(p_poi, export.now);
    let headroom_import = headroom(-p_poi, import.now);
    let up = step(p_poi, export, headroom_export, headroom_import, dt);
    let down = step(-p_poi, import, headroom_import, headroom_export, dt);
    let ceiling = u + up;
    // Reason: they only cross when a tightening jump on one side outruns the
    // rate limit on the other; that rate limit isn't a real bound, so let
    // the jump win, export side first (reverse power at the POI trips).
    let floor = (u - down).min(ceiling);
    Bounds {
        ceiling,
        floor,
        headroom_export,
        headroom_import,
    }
}

/// Room left before `limit`, for POI flow `signed_poi` in the direction it
/// caps. Infinite when there's no limit.
fn headroom(signed_poi: f64, limit: Option<f64>) -> f64 {
    limit.map_or(f64::INFINITY, |l| signed_poi + l)
}

/// How far output may move toward one limit this tick (negative = must back
/// off). `opposite` is the other limit's headroom.
fn step(signed_poi: f64, limit: Limit, headroom: f64, opposite: f64, dt: Duration) -> f64 {
    if limit.now.is_none() {
        return f64::INFINITY;
    }
    // Reason: a tightening is our own, unlagged knowledge, so the extra
    // violation it creates is taken off in one step. Only what the meter
    // already showed goes through the gain.
    let before = limit.prev.map_or(f64::INFINITY, |p| signed_poi + p);
    let jump = (before.min(0.0) - headroom.min(0.0)).max(0.0);
    let measured = headroom + jump;
    let rate = if measured >= 0.0 {
        APPROACH_GAIN_PER_SEC * measured
    } else {
        // Reason: backing off is an approach toward the opposite limit, and
        // the meter lags that just the same. Go fast only as far as the
        // approach gain allows on the opposite side; never slower than it.
        let excess = -measured;
        -(VIOLATION_GAIN_PER_SEC * excess)
            .min(APPROACH_GAIN_PER_SEC * opposite)
            .max(APPROACH_GAIN_PER_SEC * excess)
    };
    rate * dt.as_secs_f64() - jump
}
