//! Only a real change to the locked set swaps it.

use super::apply;
use crate::loto::LockedDevices;
use std::collections::HashSet;

fn set(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

#[tokio::test]
async fn a_new_lock_is_applied() {
    // Arrange
    let locked = LockedDevices::default();
    // Act
    let changed = apply(&locked, set(&["rack_1"])).await;
    // Assert
    assert!(changed);
    assert_eq!(*locked.read().await, set(&["rack_1"]));
}

#[tokio::test]
async fn a_beacon_that_changed_nothing_is_a_no_op() {
    // Arrange
    let locked = LockedDevices::default();
    apply(&locked, set(&["rack_1"])).await;
    // Act
    let changed = apply(&locked, set(&["rack_1"])).await;
    // Assert
    assert!(!changed);
}

#[tokio::test]
async fn a_cleared_lock_is_released() {
    let locked = LockedDevices::default();
    apply(&locked, set(&["rack_1"])).await;
    assert!(apply(&locked, set(&[])).await);
    assert!(locked.read().await.is_empty());
}
