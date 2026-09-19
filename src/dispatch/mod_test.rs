//! Unit tests for the dispatch lifecycle contract (event payload shape,
//! command-frame parsing).

use super::{CommandFrame, Phase, event_payload};

#[test]
fn event_payload_carries_contract_fields() {
    // Arrange + Act
    let done = event_payload("2026-07-03T00:00:00Z", "cmd-1", Phase::Done, None);
    let failed = event_payload(
        "2026-07-03T00:00:00Z",
        "cmd-2",
        Phase::Failed,
        Some("unknown device x"),
    );
    // Assert — exact wire contract per dispatchEvents.ts
    let d: serde_json::Value = serde_json::from_str(&done).unwrap();
    assert_eq!(d["phase"], "done");
    assert_eq!(d["command_id"], "cmd-1");
    assert!(d.get("reason").is_none());
    let f: serde_json::Value = serde_json::from_str(&failed).unwrap();
    assert_eq!(f["phase"], "failed");
    assert_eq!(f["reason"], "unknown device x");
}

#[test]
fn command_frame_requires_command_id() {
    // Arrange — frame missing command_id (HMI can't correlate an ack)
    let bad = br#"{"ts":"t","value":1.0}"#;
    // Act + Assert
    assert!(serde_json::from_slice::<CommandFrame>(bad).is_err());
}
