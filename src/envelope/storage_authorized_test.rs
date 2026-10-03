//! The operator's choice of which resource answers the envelope.

use super::{topic, withheld};
use crate::synthetic::new_input_cache;
use serde_json::json;
use std::time::Instant;

#[test]
fn storage_is_authorized_until_the_operator_says_otherwise() {
    // Arrange
    let cache = new_input_cache();
    // Act + Assert — nothing published yet: the battery answers
    assert!(!withheld(&cache, "s"));
    cache.insert(topic("s"), (json!(true), Instant::now()));
    assert!(!withheld(&cache, "s"));
    cache.insert(topic("s"), (json!(false), Instant::now()));
    assert!(withheld(&cache, "s"));
}

#[test]
fn the_topic_is_der_dispatch_storage_authorized() {
    assert_eq!(
        topic("device_demo_site"),
        "sites/device_demo_site/devices/der_dispatch/measurements/storage_authorized/none"
    );
}
