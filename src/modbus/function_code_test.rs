//! Function codes: what a binding may read or write with.

use super::{ReadFunction, WriteFunction, read_function, write_function};
use crate::modbus::codec::ModbusDataType;

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
