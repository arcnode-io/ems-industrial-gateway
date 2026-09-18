//! Unit tests for the pure decode/scale helpers. High-risk: silent
//! wrong-value bug if word order or scale are off.

use super::client::{WordOrder, apply_scale_offset, decode_int32, encode_int32, to_raw};

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

#[test]
fn encode_int32_high_low() {
    // int32 1_000_000 = 0x000F4240 → words [0x000F, 0x4240]
    let words = encode_int32(1_000_000, WordOrder::HighLow);
    assert_eq!(words, [0x000F, 0x4240]);
}

#[test]
fn encode_decode_int32_round_trips() {
    let value = -3_800_000;
    let words = encode_int32(value, WordOrder::HighLow);
    assert_eq!(decode_int32(&words, WordOrder::HighLow), value);
}

#[test]
fn to_raw_identity() {
    let result = to_raw(1_000_000.0, 1.0, 0.0);
    assert_eq!(result, 1_000_000);
}

#[test]
fn to_raw_apply_scale_offset_round_trips() {
    let raw = apply_scale_offset(7_000_000, 2.0, 5.0);
    assert_eq!(to_raw(raw, 2.0, 5.0), 7_000_000);
}
