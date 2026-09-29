//! e2e: the Modbus client honours the binding's function code. High-risk:
//! a device that keeps a value in input registers (FC4) returns a different
//! (or no) value from the same holding address, and a device that only
//! accepts FC6 rejects an FC16 write, so the setpoint never lands.

use anyhow::Result;
use ems_industrial_gateway::asyncapi::types::ModbusTcpBinding;
use ems_industrial_gateway::modbus::client::{
    ModbusDataType, WordOrder, read_measurement, write_setpoint,
};
use rodbus::server::{
    AddressFilter, RequestHandler, ServerHandle, ServerHandlerMap, WriteRegisters,
    spawn_tcp_server_task,
};
use rodbus::{DecodeLevel, ExceptionCode, Indexed, UnitId};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

const ADDR: u16 = 100;
const HOLDING_VALUE: u16 = 111;
const INPUT_VALUE: u16 = 222;

/// Same address, different value per register space; records how each write
/// arrived so the test can tell FC6 from FC16.
#[derive(Default)]
struct Recorder {
    single_writes: Vec<Indexed<u16>>,
    multiple_writes: usize,
}

impl RequestHandler for Recorder {
    fn read_holding_register(&self, address: u16) -> Result<u16, ExceptionCode> {
        (address == ADDR)
            .then_some(HOLDING_VALUE)
            .ok_or(ExceptionCode::IllegalDataAddress)
    }
    fn read_input_register(&self, address: u16) -> Result<u16, ExceptionCode> {
        (address == ADDR)
            .then_some(INPUT_VALUE)
            .ok_or(ExceptionCode::IllegalDataAddress)
    }
    fn write_single_register(&mut self, value: Indexed<u16>) -> Result<(), ExceptionCode> {
        self.single_writes.push(value);
        Ok(())
    }
    fn write_multiple_registers(&mut self, _values: WriteRegisters) -> Result<(), ExceptionCode> {
        self.multiple_writes += 1;
        Ok(())
    }
}

async fn spawn() -> Result<(SocketAddr, Arc<Mutex<Box<Recorder>>>, ServerHandle)> {
    // Reason: bind-and-drop to learn a free port; rodbus takes a concrete addr.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let addr = listener.local_addr()?;
    drop(listener);
    let handler = Recorder::default().wrap();
    let map = ServerHandlerMap::single(UnitId::new(1), handler.clone());
    let server =
        spawn_tcp_server_task(4, addr, map, AddressFilter::Any, DecodeLevel::default()).await?;
    Ok((addr, handler, server))
}

fn binding(addr: SocketAddr, function_code: u8) -> ModbusTcpBinding {
    ModbusTcpBinding {
        host: addr.ip().to_string(),
        port: addr.port(),
        unit_id: "1".to_string(),
        address: ADDR,
        scale: 1.0,
        offset: 0.0,
        data_type: ModbusDataType::Uint16,
        word_order: WordOrder::HighLow,
        function_code: Some(function_code),
    }
}

#[tokio::test]
async fn fc4_reads_input_registers_and_fc3_reads_holding() -> Result<()> {
    // Arrange
    let (addr, _handler, _server) = spawn().await?;
    // Act
    let input = read_measurement(&binding(addr, 4), None, None).await?;
    let holding = read_measurement(&binding(addr, 3), None, None).await?;
    // Assert
    assert_eq!(input, f64::from(INPUT_VALUE));
    assert_eq!(holding, f64::from(HOLDING_VALUE));
    Ok(())
}

#[tokio::test]
async fn fc6_writes_a_single_register() -> Result<()> {
    // Arrange
    let (addr, handler, _server) = spawn().await?;
    // Act
    write_setpoint(&binding(addr, 6), 42.0, None, None).await?;
    // Assert
    let recorded = handler.lock().unwrap();
    assert_eq!(
        recorded.single_writes,
        vec![Indexed {
            index: ADDR,
            value: 42
        }]
    );
    assert_eq!(recorded.multiple_writes, 0);
    Ok(())
}
