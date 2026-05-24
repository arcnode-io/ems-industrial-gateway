//! Testcontainer + in-process fixtures for the e2e integration tests.

// Reason: per-test-binary modules — each `tests/*.rs` consumes a subset.
// `#[allow(dead_code)]` silences "unused" warnings on the other binary.
#[allow(dead_code)]
pub mod containers;
#[allow(dead_code)]
pub mod dnp3_security;
#[allow(dead_code)]
pub mod modbus_security;
#[allow(dead_code)]
pub mod pki;
#[allow(dead_code)]
pub mod redfish_security;
#[allow(dead_code)]
pub mod spec_stub;
