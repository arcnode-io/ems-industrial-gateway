//! The gateway takes device-api's expanded set as is.

use super::parse;
use std::collections::HashSet;

#[test]
fn the_locked_set_is_device_apis_expanded_list() {
    // Arrange — one lock on a module; device-api expanded its racks
    let body = r#"{
        "locks": [{
            "id": 12, "device_id": "bess_module_1", "holder_name": "J. Narvaez",
            "permit_ref": null, "set_at": "2026-10-10T19:04:12.345Z",
            "set_by_role": "operator", "cleared_at": null, "cleared_by_role": null
        }],
        "locked_devices": ["bess_module_1", "bess_rack_1", "bess_rack_2"]
    }"#;
    // Act
    let locked = parse(body).unwrap();
    // Assert
    let expected: HashSet<String> = ["bess_module_1", "bess_rack_1", "bess_rack_2"]
        .map(String::from)
        .into();
    assert_eq!(locked, expected);
}

#[test]
fn a_body_without_the_list_is_an_error() {
    assert!(parse(r#"{"locks":[]}"#).is_err());
}
