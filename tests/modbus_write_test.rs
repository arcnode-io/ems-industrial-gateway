//! e2e: real Modbus TCP write round-trip against mock-modbus-server's
//! writable mode. Proves the value that lands on the wire is correct — not
//! just that the protocol write was accepted — by reading the same register
//! back afterward and decoding it.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::asyncapi::types::ModbusTcpBinding;
use ems_industrial_gateway::modbus::client::{
    ModbusDataType, WordOrder, decode_int32, read_holding,
};
use fixtures::containers::start_mock_modbus_server_writable;

/// bess_rack's set_active_power binding: function code 16, address 50,
/// int32 high_low, scale 1.0.
const ADDRESS: u16 = 50;

#[tokio::test]
async fn write_setpoint_lands_the_exact_value_on_the_wire() -> Result<()> {
    // Arrange — real writable mock-modbus-server container.
    let mock = start_mock_modbus_server_writable().await?;
    let port = mock.get_host_port_ipv4(502).await?;
    let binding = ModbusTcpBinding {
        host: "127.0.0.1".to_string(),
        port,
        unit_id: "1".to_string(),
        address: ADDRESS,
        scale: 1.0,
        offset: 0.0,
        data_type: ModbusDataType::Int32,
        word_order: WordOrder::HighLow,
    };

    // Act — write a setpoint through the same path dispatch::handle_command uses.
    ems_industrial_gateway::modbus::client::write_setpoint(&binding, 1_620_000.0, None, None)
        .await?;

    // Assert — reading the same register back decodes to the exact value written.
    let words = read_holding("127.0.0.1", port, 1, ADDRESS, 2).await?;
    let raw = decode_int32(&words, WordOrder::HighLow);
    assert_eq!(raw, 1_620_000);

    Ok(())
}

#[tokio::test]
async fn write_is_rejected_when_server_not_in_writable_mode() -> Result<()> {
    // Arrange — default (read-only) mock-modbus-server.
    let mock = fixtures::containers::start_mock_modbus_server().await?;
    let port = mock.get_host_port_ipv4(502).await?;
    let binding = ModbusTcpBinding {
        host: "127.0.0.1".to_string(),
        port,
        unit_id: "1".to_string(),
        address: ADDRESS,
        scale: 1.0,
        offset: 0.0,
        data_type: ModbusDataType::Int32,
        word_order: WordOrder::HighLow,
    };

    // Act + Assert — the default mock still rejects writes (IllegalFunction).
    let result =
        ems_industrial_gateway::modbus::client::write_setpoint(&binding, 1.0, None, None).await;
    assert!(result.is_err(), "read-only mock must reject the write");

    Ok(())
}
