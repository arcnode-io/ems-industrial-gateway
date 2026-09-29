//! Rate-limited estimate of site load at the POI.
//!
//! Reason: site load is computed as `P_poi + P_bess`, but the meter's reading
//! lags the battery's by 1–2 s. Unfiltered, every battery step is briefly
//! counted as load (and then as negative load), and the envelope's instant
//! clamp turns that into an oscillation that can export the whole site
//! load. A site's load moves slowly, so bounding how fast the estimate can
//! move filters out the battery's own steps while still tracking real load
//! changes within seconds.

use std::time::Duration;

/// Site load estimate that can move at most `max_ramp_w_per_s`.
pub struct SiteLoadEstimate {
    /// Maximum rate of change, W/s.
    max_ramp_w_per_s: f64,
    /// Current estimate; `None` until the first reading.
    value: Option<f64>,
}

impl SiteLoadEstimate {
    /// Build an estimate with no reading yet.
    #[must_use]
    pub fn new(max_ramp_w_per_s: f64) -> Self {
        Self {
            max_ramp_w_per_s,
            value: None,
        }
    }

    /// Fold in a raw `P_poi + P_bess` reading taken `dt` after the last one.
    pub fn update(&mut self, raw: f64, dt: Duration) -> f64 {
        let next = match self.value {
            None => raw,
            Some(prev) => {
                let step = self.max_ramp_w_per_s * dt.as_secs_f64();
                prev + (raw - prev).clamp(-step, step)
            }
        };
        self.value = Some(next);
        next
    }
}
