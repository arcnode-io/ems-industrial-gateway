//! Keeping the envelope controller's output to what distribution delivers.

use crate::envelope::control_law::EnvelopeController;

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
