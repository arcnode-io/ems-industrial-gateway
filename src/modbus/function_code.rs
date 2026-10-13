//! Which Modbus function code a binding reads or writes with, checked
//! against its data type at spec parse.

use crate::modbus::codec::ModbusDataType;

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

#[cfg(test)]
#[path = "function_code_test.rs"]
mod tests;
