//! der_dispatch posture: dispatch only a setpoint that's actually in force.

use super::{Posture, PostureTopics, posture};
use crate::synthetic::{InputCache, new_input_cache};
use serde_json::{Value, json};
use std::time::Instant;

fn topics() -> PostureTopics {
    PostureTopics {
        event_active: "event_active".to_string(),
        target_present: "target_present".to_string(),
        target: "target".to_string(),
    }
}

/// A cache holding whichever of (event_active, target_present, target) are `Some`.
fn retained(event: Option<Value>, present: Option<Value>, target: Option<f64>) -> InputCache {
    let cache = new_input_cache();
    let now = Instant::now();
    for (topic, value) in [
        ("event_active", event),
        ("target_present", present),
        ("target", target.map(|t| json!(t))),
    ] {
        if let Some(v) = value {
            cache.insert(topic.to_string(), (v, now));
        }
    }
    cache
}

#[test]
fn a_commanded_setpoint_is_dispatched() {
    let cache = retained(Some(json!(true)), Some(json!(true)), Some(400_000.0));
    assert_eq!(posture(&topics(), &cache), Posture::Dispatch(400_000.0));
}

#[test]
fn a_commanded_zero_is_still_a_full_curtailment() {
    let cache = retained(Some(json!(true)), Some(json!(true)), Some(0.0));
    assert_eq!(posture(&topics(), &cache), Posture::Dispatch(0.0));
}

#[test]
fn an_energize_only_event_releases_rather_than_dispatching_zero() {
    // Arrange — active, but no power setpoint in force
    let cache = retained(Some(json!(true)), Some(json!(false)), Some(0.0));
    // Act + Assert
    assert_eq!(posture(&topics(), &cache), Posture::Release);
}

#[test]
fn no_event_releases() {
    let cache = retained(Some(json!(false)), Some(json!(false)), Some(0.0));
    assert_eq!(posture(&topics(), &cache), Posture::Release);
}

#[test]
fn anything_unpublished_during_an_event_holds() {
    let t = topics();
    assert_eq!(posture(&t, &retained(None, None, None)), Posture::Hold);
    let no_present = retained(Some(json!(true)), None, Some(400_000.0));
    assert_eq!(posture(&t, &no_present), Posture::Hold);
    let no_target = retained(Some(json!(true)), Some(json!(true)), None);
    assert_eq!(posture(&t, &no_target), Posture::Hold);
}
