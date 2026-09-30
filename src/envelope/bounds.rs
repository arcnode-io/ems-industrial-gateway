//! Where the controller's output may go this tick: POI-referenced (via
//! `poi_servo`) when the site has a POI meter, battery-only otherwise.

use crate::envelope::control_law::EnvelopeTick;
use crate::envelope::poi_servo::{self, Limit};

/// One tick's output bounds and the headroom they came from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    /// Highest output (discharge) allowed this tick.
    pub ceiling: f64,
    /// Lowest output (charge) allowed this tick.
    pub floor: f64,
    /// Export headroom (≤ 0 = violating export_limit).
    pub headroom_export: f64,
    /// Import headroom (≤ 0 = violating import_limit).
    pub headroom_import: f64,
}

/// This tick's bounds for commanded output `u`. `prev_limits` is last
/// tick's (import, export), which the POI servo needs to spot a tightening.
#[must_use]
pub fn for_tick(u: f64, prev_limits: (Option<f64>, Option<f64>), t: &EnvelopeTick) -> Bounds {
    match t.poi_active_power {
        Some(p_poi) => poi_servo::bounds(
            u,
            p_poi,
            Limit {
                now: t.import_limit,
                prev: prev_limits.0,
            },
            Limit {
                now: t.export_limit,
                prev: prev_limits.1,
            },
            t.dt,
        ),
        // No POI meter: the battery is the only asset, so its own reading
        // is the POI flow and the limits bound it directly.
        None => {
            let ceiling = t.export_limit.unwrap_or(f64::INFINITY);
            let floor = t.import_limit.map_or(f64::NEG_INFINITY, |i| -i);
            Bounds {
                ceiling,
                floor,
                headroom_export: ceiling - t.active_power,
                headroom_import: t.active_power - floor,
            }
        }
    }
}
