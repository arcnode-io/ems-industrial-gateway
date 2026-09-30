//! e2e: a SunSpec measurement reads its scale factor register at runtime.
//! High-risk: SunSpec inverters (model 103) store W as an int16 whose decimal
//! exponent lives in W_SF; ignoring it reads 123.4 W as 1234 W.

use anyhow::Result;
use ems_industrial_gateway::asyncapi::types::ModbusTcpBinding;
use ems_industrial_gateway::modbus::client::read_measurement;
use rodbus::server::{
    AddressFilter, RequestHandler, ServerHandle, ServerHandlerMap, spawn_tcp_server_task,
};
use rodbus::{DecodeLevel, ExceptionCode, UnitId};
use serde_json::json;
use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::TcpListener;

/// Model 103 W and W_SF.
const W: u16 = 40084;
const W_SF: u16 = 40085;

/// W = 1234, W_SF = -1 (0xFFFF) → 123.4 W.
struct Inverter;

impl RequestHandler for Inverter {
    fn read_holding_register(&self, address: u16) -> Result<u16, ExceptionCode> {
        match address {
            W => Ok(1234),
            W_SF => Ok(0xFFFF),
            _ => Err(ExceptionCode::IllegalDataAddress),
        }
    }
}

async fn spawn() -> Result<(SocketAddr, ServerHandle)> {
    // Reason: bind-and-drop to learn a free port; rodbus takes a concrete addr.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let addr = listener.local_addr()?;
    drop(listener);
    let map = ServerHandlerMap::single(UnitId::new(1), Inverter.wrap());
    let server =
        spawn_tcp_server_task(4, addr, map, AddressFilter::Any, DecodeLevel::default()).await?;
    Ok((addr, server))
}

#[tokio::test]
async fn a_sunssf_binding_applies_the_scale_factor_register() -> Result<()> {
    // Arrange
    let (addr, _server) = spawn().await?;
    let binding: ModbusTcpBinding = serde_json::from_value(json!({
        "host": addr.ip().to_string(), "port": addr.port(), "unit_id": "1",
        "address": W, "scale": 1.0, "offset": 0.0, "data_type": "int16",
        "function_code": 3, "scale_factor_address": W_SF,
    }))?;
    // Act
    let watts = read_measurement(&binding, None, None).await?;
    // Assert
    assert!((watts - 123.4).abs() < 1e-9, "read {watts} W");
    Ok(())
}
