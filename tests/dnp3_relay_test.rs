//! e2e: a protective relay's DNP3 map mixes analog and binary inputs. High-
//! risk: trip and ground-fault targets are binary inputs; reading only
//! analogs leaves the protection state dark on the HMI.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::asyncapi::types::Dnp3TcpBinding;
use ems_industrial_gateway::dnp3::client::read_measurement;
use fixtures::dnp3_relay::{GROUND_FAULT, PHASE_VOLTAGE_A, PHASE_VOLTAGE_A_KV, TRIP_STATUS, spawn};
use serde_json::json;
use std::net::SocketAddr;

fn binding(addr: SocketAddr, point_index: u16, point_type: &str, scale: f64) -> Dnp3TcpBinding {
    serde_json::from_value(json!({
        "host": addr.ip().to_string(), "port": addr.port(),
        "point_index": point_index, "point_type": point_type, "scale": scale,
    }))
    .unwrap()
}

#[tokio::test]
async fn binary_inputs_read_as_one_or_zero() -> Result<()> {
    // Arrange
    let (addr, _server) = spawn().await?;
    // Act
    let trip =
        read_measurement(&binding(addr, TRIP_STATUS, "binary_input", 1.0), None, None).await?;
    let ground = read_measurement(
        &binding(addr, GROUND_FAULT, "binary_input", 1.0),
        None,
        None,
    )
    .await?;
    // Assert
    assert_eq!(trip, 1.0);
    assert_eq!(ground, 0.0);
    Ok(())
}

#[tokio::test]
async fn an_analog_input_in_kilovolts_scales_to_volts() -> Result<()> {
    // Arrange
    let (addr, _server) = spawn().await?;
    // Act
    let volts = read_measurement(
        &binding(addr, PHASE_VOLTAGE_A, "analog_input", 1000.0),
        None,
        None,
    )
    .await?;
    // Assert — within float32 wire precision (Var 5 is a 32-bit float)
    assert!(
        (volts - PHASE_VOLTAGE_A_KV * 1000.0).abs() < 0.01,
        "read {volts} V"
    );
    Ok(())
}
