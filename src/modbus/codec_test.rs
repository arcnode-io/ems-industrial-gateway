//! Unit tests for data-type-aware decode/encode. High-risk: a real device's
//! register width mismatched against the binding silently misreads its
//! neighbor register (or, for a 1-register type, throws IllegalDataAddress
//! reading past the end of the device's map).

use super::codec::{ModbusDataType, WordOrder, decode_raw, encode_raw};

#[test]
fn register_count_matches_wire_width() {
    assert_eq!(ModbusDataType::Uint16.register_count(), 1);
    assert_eq!(ModbusDataType::Int16.register_count(), 1);
    assert_eq!(ModbusDataType::Int32.register_count(), 2);
    assert_eq!(ModbusDataType::Uint32.register_count(), 2);
    assert_eq!(ModbusDataType::Float32.register_count(), 2);
}

#[test]
fn decode_raw_uint16_single_register() {
    let value = decode_raw(&[700], ModbusDataType::Uint16, WordOrder::HighLow);
    assert_eq!(value, 700.0);
}

#[test]
fn decode_raw_int16_negative_single_register() {
    // -1 as i16 = 0xFFFF.
    let value = decode_raw(&[0xFFFF], ModbusDataType::Int16, WordOrder::HighLow);
    assert_eq!(value, -1.0);
}

#[test]
fn decode_raw_int32_matches_existing_behavior() {
    // int32 1_000_000 = 0x000F4240 -> words [0x000F, 0x4240].
    let value = decode_raw(&[0x000F, 0x4240], ModbusDataType::Int32, WordOrder::HighLow);
    assert_eq!(value, 1_000_000.0);
}

#[test]
fn decode_raw_uint32_high_low() {
    let value = decode_raw(
        &[0x0001, 0x0000],
        ModbusDataType::Uint32,
        WordOrder::HighLow,
    );
    assert_eq!(value, 65536.0);
}

#[test]
fn decode_raw_float32_ieee754_bit_pattern() {
    // 1.5f32 = 0x3FC00000.
    let value = decode_raw(
        &[0x3FC0, 0x0000],
        ModbusDataType::Float32,
        WordOrder::HighLow,
    );
    assert_eq!(value, 1.5);
}

#[test]
fn encode_decode_raw_round_trips_for_every_data_type() {
    for (data_type, raw) in [
        (ModbusDataType::Uint16, 700.0),
        (ModbusDataType::Int16, -1.0),
        (ModbusDataType::Int32, 1_000_000.0),
        (ModbusDataType::Uint32, 65536.0),
        (ModbusDataType::Float32, 1.5),
    ] {
        let words = encode_raw(raw, data_type, WordOrder::HighLow);
        assert_eq!(
            decode_raw(&words, data_type, WordOrder::HighLow),
            raw,
            "{data_type:?} round-trip"
        );
    }
}
