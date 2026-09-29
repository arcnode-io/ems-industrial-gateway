//! Pure Modbus register decode/encode — no I/O. Wire-width (`ModbusDataType`)
//! and register order (`WordOrder`) live here since they're protocol
//! framing concerns, not asyncapi schema concerns; `asyncapi::types::bindings`
//! imports them for `ModbusTcpBinding`'s fields.

use serde::{Deserialize, Serialize};

/// Word order for multi-register decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WordOrder {
    /// High word first (AB CD).
    #[default]
    HighLow,
    /// Low word first (CD AB).
    LowHigh,
}

/// Register-level data type a Modbus binding decodes/encodes as. Default is
/// `Int32` — matches every binding that predates this field (all existing
/// real + stub bindings are int32), so an AsyncAPI payload or hand-rolled
/// test stub that omits `data_type` keeps its current 2-register behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModbusDataType {
    /// Signed 16-bit, 1 register.
    Int16,
    /// Unsigned 16-bit, 1 register.
    Uint16,
    /// Signed 32-bit, 2 registers.
    #[default]
    Int32,
    /// Unsigned 32-bit, 2 registers.
    Uint32,
    /// IEEE-754 single-precision float, 2 registers.
    Float32,
    /// Signed 64-bit, 4 registers (e.g. ION9000 energy counters in Wh).
    Int64,
}

impl ModbusDataType {
    /// How many consecutive 16-bit holding registers this type spans.
    pub fn register_count(self) -> u16 {
        match self {
            ModbusDataType::Int16 | ModbusDataType::Uint16 => 1,
            ModbusDataType::Int32 | ModbusDataType::Uint32 | ModbusDataType::Float32 => 2,
            ModbusDataType::Int64 => 4,
        }
    }
}

/// Decode two consecutive u16 holding registers as an unsigned 32-bit int.
pub fn decode_uint32(words: &[u16], order: WordOrder) -> u32 {
    let (high, low) = match order {
        WordOrder::HighLow => (words[0], words[1]),
        WordOrder::LowHigh => (words[1], words[0]),
    };
    ((high as u32) << 16) | (low as u32)
}

/// Encode an unsigned 32-bit int as two consecutive u16 holding registers.
pub fn encode_uint32(value: u32, order: WordOrder) -> [u16; 2] {
    let high = (value >> 16) as u16;
    let low = value as u16;
    match order {
        WordOrder::HighLow => [high, low],
        WordOrder::LowHigh => [low, high],
    }
}

/// Decode two consecutive u16 holding registers as a signed 32-bit integer.
pub fn decode_int32(words: &[u16], order: WordOrder) -> i32 {
    decode_uint32(words, order) as i32
}

/// Encode a signed 32-bit integer as two consecutive u16 holding registers.
pub fn encode_int32(value: i32, order: WordOrder) -> [u16; 2] {
    encode_uint32(value as u32, order)
}

/// Decode four consecutive u16 holding registers as a signed 64-bit integer.
/// `HighLow` = big-endian word order (word 0 most significant, per the
/// ION9000 map); `LowHigh` reverses all four words.
pub fn decode_int64(words: &[u16], order: WordOrder) -> i64 {
    let ordered = match order {
        WordOrder::HighLow => [words[0], words[1], words[2], words[3]],
        WordOrder::LowHigh => [words[3], words[2], words[1], words[0]],
    };
    ordered
        .iter()
        .fold(0u64, |acc, w| (acc << 16) | u64::from(*w)) as i64
}

/// Encode a signed 64-bit integer as four consecutive u16 holding registers.
pub fn encode_int64(value: i64, order: WordOrder) -> [u16; 4] {
    let raw = value as u64;
    let big_endian = [
        (raw >> 48) as u16,
        (raw >> 32) as u16,
        (raw >> 16) as u16,
        raw as u16,
    ];
    match order {
        WordOrder::HighLow => big_endian,
        WordOrder::LowHigh => [big_endian[3], big_endian[2], big_endian[1], big_endian[0]],
    }
}

/// Decode a measurement's raw (pre-scale) numeric value per its data type.
/// `words` must hold at least `data_type.register_count()` entries.
pub fn decode_raw(words: &[u16], data_type: ModbusDataType, order: WordOrder) -> f64 {
    match data_type {
        ModbusDataType::Uint16 => words[0] as f64,
        ModbusDataType::Int16 => (words[0] as i16) as f64,
        ModbusDataType::Int32 => decode_int32(words, order) as f64,
        ModbusDataType::Uint32 => decode_uint32(words, order) as f64,
        ModbusDataType::Float32 => f32::from_bits(decode_uint32(words, order)) as f64,
        ModbusDataType::Int64 => decode_int64(words, order) as f64,
    }
}

/// Encode a raw (pre-scale) numeric value into the registers its data type
/// spans. Inverse of `decode_raw`.
pub fn encode_raw(raw: f64, data_type: ModbusDataType, order: WordOrder) -> Vec<u16> {
    match data_type {
        ModbusDataType::Uint16 => vec![raw.round() as u16],
        ModbusDataType::Int16 => vec![(raw.round() as i16) as u16],
        ModbusDataType::Int32 => encode_int32(raw.round() as i32, order).to_vec(),
        ModbusDataType::Uint32 => encode_uint32(raw.round() as u32, order).to_vec(),
        ModbusDataType::Float32 => encode_uint32((raw as f32).to_bits(), order).to_vec(),
        ModbusDataType::Int64 => encode_int64(raw.round() as i64, order).to_vec(),
    }
}

/// Which register space a measurement reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadFunction {
    /// Function code 3, holding registers.
    Holding,
    /// Function code 4, input registers.
    Input,
}

/// How a command writes its registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteFunction {
    /// Function code 6, write single register.
    Single,
    /// Function code 16, write multiple registers.
    Multiple,
}

/// A measurement binding's read function code. Absent = 3 (holding), which
/// every binding predating the field used.
pub fn read_function(code: Option<u8>) -> Result<ReadFunction, String> {
    match code {
        None | Some(3) => Ok(ReadFunction::Holding),
        Some(4) => Ok(ReadFunction::Input),
        Some(other) => Err(format!(
            "function_code {other} is not a register read (3 or 4)"
        )),
    }
}

/// A command binding's write function code, checked against its data type.
/// Absent = 16 (write multiple), which every binding predating the field used.
/// FC6 writes exactly one register, so a wider type would be truncated.
pub fn write_function(
    code: Option<u8>,
    data_type: ModbusDataType,
) -> Result<WriteFunction, String> {
    match code {
        None | Some(16) => Ok(WriteFunction::Multiple),
        Some(6) if data_type.register_count() == 1 => Ok(WriteFunction::Single),
        Some(6) => Err(format!(
            "function_code 6 writes one register but {data_type:?} spans {}",
            data_type.register_count()
        )),
        Some(other) => Err(format!(
            "function_code {other} is not a register write (6 or 16)"
        )),
    }
}

/// Apply Modbus scale + offset to a raw reading.
pub fn apply_scale_offset(raw: f64, scale: f64, offset: f64) -> f64 {
    raw * scale + offset
}

/// Invert `apply_scale_offset`: convert an engineering-unit setpoint back to
/// the raw value a Modbus write puts on the wire.
pub fn to_raw(value: f64, scale: f64, offset: f64) -> f64 {
    (value - offset) / scale
}
