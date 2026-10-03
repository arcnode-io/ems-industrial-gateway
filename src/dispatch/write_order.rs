//! Order of child writes when shares move between children, so a handoff
//! never briefly puts both children on the bus.

use std::collections::HashMap;

/// Order child writes so every share that shrinks (in magnitude) lands
/// before any that grows. `last` holds each child's last written share; a
/// child never written counts as 0.
///
/// Reason: writes land one device at a time. Raising one child before
/// lowering another briefly puts both on the bus at once, which a POI meter
/// sees as export (or import). Lowering first only under-delivers briefly.
pub fn reductions_first(
    mut changed: Vec<(String, f64)>,
    last: &HashMap<String, f64>,
) -> Vec<(String, f64)> {
    changed.sort_by(|a, b| growth(a, last).total_cmp(&growth(b, last)));
    changed
}

/// Smallest shrink, as a fraction of the child's output, that is a handoff.
const HANDOFF_MIN_FRACTION: f64 = 0.01;

/// Which of this tick's (already `reductions_first`-ordered) child writes
/// to send, and whether growth was held back. When a share materially
/// shrinks while another grows, only the shrinks go now and the growth
/// waits one tick.
pub fn handoff_batch(
    changed: Vec<(String, f64)>,
    last: &HashMap<String, f64>,
    deferred_last_tick: bool,
) -> (Vec<(String, f64)>, bool) {
    // Reason: each rack takes its setpoint with its own latency (up to a
    // tick), so a raise written right after a drop can still land first.
    // Holding growth a tick lets the drop take effect: a moment of
    // under-delivery instead of both racks on the bus. At most one tick —
    // SoC drift shrinks some share nearly every tick and would otherwise
    // starve the growing rack.
    let shrinks = changed.iter().any(|c| material_shrink(c, last));
    let grows = changed.iter().any(|c| growth(c, last) > 0.0);
    if shrinks && grows && !deferred_last_tick {
        let batch = changed
            .into_iter()
            .filter(|c| growth(c, last) < 0.0)
            .collect();
        return (batch, true);
    }
    (changed, false)
}

/// A shrink big enough to be a handoff: more than `HANDOFF_MIN_FRACTION` of
/// the child's last output. Reason: SoC drift moves a few hundred watts
/// between racks nearly every tick. Deferring those re-armed the envelope's
/// approach hold each tick and froze the battery 355 kW short on the demo,
/// for an overlap risk no bigger than the drift itself.
fn material_shrink(c: &(String, f64), last: &HashMap<String, f64>) -> bool {
    let previous = last.get(&c.0).map_or(0.0, |p| p.abs());
    -growth(c, last) > HANDOFF_MIN_FRACTION * previous
}

/// How much a child's write grows its output magnitude; a child never
/// written counts as 0.
fn growth((id, share): &(String, f64), last: &HashMap<String, f64>) -> f64 {
    share.abs() - last.get(id).map_or(0.0, |p| p.abs())
}
