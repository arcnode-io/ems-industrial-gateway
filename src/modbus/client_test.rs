//! Unit tests for the pure decode/scale helpers. High-risk: silent
//! wrong-value bug if word order or scale are off.

use super::client::{WordOrder, apply_scale_offset, decode_int32};

#[test]
fn decode_int32_high_low() {
    // int32 1_000_000 = 0x000F4240 → words [0x000F, 0x4240]
    let value = decode_int32(&[0x000F, 0x4240], WordOrder::HighLow);
    assert_eq!(value, 1_000_000);
}

#[test]
fn apply_scale_offset_identity() {
    let result = apply_scale_offset(1_000_000, 1.0, 0.0);
    assert_eq!(result, 1_000_000.0);
}
