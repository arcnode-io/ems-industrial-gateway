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

#[test]
fn int64_spans_four_registers() {
    assert_eq!(ModbusDataType::Int64.register_count(), 4);
}

#[test]
fn int64_high_low_is_big_endian_word_order() {
    // 0x0000_0001_0002_0003 — word 0 most significant (ION9000 map).
    let value = decode_raw(&[0, 1, 2, 3], ModbusDataType::Int64, WordOrder::HighLow);
    assert_eq!(value, ((1u64 << 32) | (2 << 16) | 3) as f64);
}

#[test]
fn int64_low_high_reverses_all_four_words() {
    let value = decode_raw(&[3, 2, 1, 0], ModbusDataType::Int64, WordOrder::LowHigh);
    assert_eq!(value, ((1u64 << 32) | (2 << 16) | 3) as f64);
}

#[test]
fn int64_negative_decodes_as_twos_complement() {
    let value = decode_raw(
        &[0xFFFF, 0xFFFF, 0xFFFF, 0xFFFE],
        ModbusDataType::Int64,
        WordOrder::HighLow,
    );
    assert_eq!(value, -2.0);
}

#[test]
fn int64_round_trips_through_encode() {
    for order in [WordOrder::HighLow, WordOrder::LowHigh] {
        let words = encode_raw(-123_456_789_012.0, ModbusDataType::Int64, order);
        assert_eq!(words.len(), 4);
        assert_eq!(
            decode_raw(&words, ModbusDataType::Int64, order),
            -123_456_789_012.0
        );
    }
}

#[test]
fn int64_parses_from_the_wire_name() {
    let parsed: ModbusDataType = serde_json::from_str(r#""int64""#).unwrap();
    assert_eq!(parsed, ModbusDataType::Int64);
}

use super::codec::{ReadFunction, WriteFunction, read_function, write_function};

#[test]
fn read_function_maps_fc3_and_fc4_and_defaults_to_holding() {
    assert_eq!(read_function(None), Ok(ReadFunction::Holding));
    assert_eq!(read_function(Some(3)), Ok(ReadFunction::Holding));
    assert_eq!(read_function(Some(4)), Ok(ReadFunction::Input));
}

#[test]
fn read_function_rejects_non_read_codes() {
    // e.g. a write code (6, 16) or a coil read (1) on a measurement
    for code in [1, 2, 6, 16] {
        assert!(
            read_function(Some(code)).is_err(),
            "fc{code} accepted as a read"
        );
    }
}

#[test]
fn write_function_maps_fc6_and_fc16_and_defaults_to_multiple() {
    assert_eq!(
        write_function(None, ModbusDataType::Int32),
        Ok(WriteFunction::Multiple)
    );
    assert_eq!(
        write_function(Some(16), ModbusDataType::Int32),
        Ok(WriteFunction::Multiple)
    );
    assert_eq!(
        write_function(Some(6), ModbusDataType::Uint16),
        Ok(WriteFunction::Single)
    );
}

#[test]
fn fc6_cannot_write_a_multi_register_type() {
    // FC6 writes exactly one register; an int32 would be silently truncated.
    assert!(write_function(Some(6), ModbusDataType::Int32).is_err());
}

#[test]
fn write_function_rejects_read_codes() {
    assert!(write_function(Some(3), ModbusDataType::Uint16).is_err());
}
