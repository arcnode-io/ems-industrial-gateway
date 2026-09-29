//! Unit tests for curtailment-event memory: what gets restored when an
//! event ends. High-risk: getting it wrong either keeps draining the
//! reserve after the utility let go, or stomps an operator's own command.

use super::EventMemory;
use std::collections::HashMap;

fn map(entries: &[(&str, f64)]) -> HashMap<String, f64> {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_string(), *v))
        .collect()
}

#[test]
fn ending_an_event_restores_the_pre_event_operator_setpoint() {
    // Arrange — operator had module_1 charging at 100 kW before the event
    let mut mem = EventMemory::default();
    mem.begin(|| map(&[("module_1", -100_000.0)]));
    mem.record("module_1", 280_000.0);
    // Act — event ends; module_1 still holds the event's setpoint
    let restore = mem.end(&map(&[("module_1", 280_000.0)]));
    // Assert
    assert_eq!(restore, vec![("module_1".to_string(), -100_000.0)]);
}

#[test]
fn a_module_with_no_pre_event_setpoint_returns_to_zero() {
    let mut mem = EventMemory::default();
    mem.begin(HashMap::new);
    mem.record("module_2", 120_000.0);
    let restore = mem.end(&map(&[("module_2", 120_000.0)]));
    assert_eq!(restore, vec![("module_2".to_string(), 0.0)]);
}

#[test]
fn an_operator_command_issued_mid_event_is_left_alone() {
    // Arrange — the event wrote 280 kW, then an operator commanded 50 kW
    let mut mem = EventMemory::default();
    mem.begin(|| map(&[("module_1", -100_000.0)]));
    mem.record("module_1", 280_000.0);
    // Act
    let restore = mem.end(&map(&[("module_1", 50_000.0)]));
    // Assert — the operator's command stands
    assert!(restore.is_empty());
}

#[test]
fn the_snapshot_is_taken_once_not_on_every_tick() {
    // Once the event has dispatched, the operator map holds the event's own
    // value; re-snapshotting then would "restore" the curtailment.
    let mut mem = EventMemory::default();
    mem.begin(|| map(&[("module_1", -100_000.0)]));
    mem.record("module_1", 280_000.0);
    mem.begin(|| map(&[("module_1", 280_000.0)]));
    let restore = mem.end(&map(&[("module_1", 280_000.0)]));
    assert_eq!(restore, vec![("module_1".to_string(), -100_000.0)]);
}

#[test]
fn no_event_means_nothing_to_restore() {
    let mem = EventMemory::default();
    assert!(mem.end(&map(&[("module_1", 280_000.0)])).is_empty());
}

#[test]
fn clearing_forgets_the_event_so_the_next_one_snapshots_afresh() {
    let mut mem = EventMemory::default();
    mem.begin(|| map(&[("module_1", -100_000.0)]));
    mem.record("module_1", 280_000.0);
    mem.clear();
    mem.begin(|| map(&[("module_1", 30_000.0)]));
    mem.record("module_1", 400_000.0);
    let restore = mem.end(&map(&[("module_1", 400_000.0)]));
    assert_eq!(restore, vec![("module_1".to_string(), 30_000.0)]);
}

#[test]
fn changed_skips_shares_already_dispatched() {
    let mut mem = EventMemory::default();
    mem.record("module_1", 280_000.0);
    let changed = mem.changed(vec![
        ("module_1".to_string(), 280_000.0),
        ("module_2".to_string(), 120_000.0),
    ]);
    assert_eq!(changed, vec![("module_2".to_string(), 120_000.0)]);
}
