//! e2e: a sunspec binding reads by model + point. The gateway finds the
//! device's SunSpec map at runtime, so a unit whose map starts somewhere
//! other than 40000 still reads right, and a point the unit reports as
//! "not implemented" is never published as a number.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::asyncapi::types::SunspecBinding;
use ems_industrial_gateway::modbus::client::channel;
use ems_industrial_gateway::sunspec::client::read_on;
use fixtures::sunspec_pcs::{Pcs, at, spawn};
use serde_json::json;

fn binding(pcs: &Pcs, model_id: u16, point: &str, bit: Option<u8>) -> SunspecBinding {
    serde_json::from_value(json!({
        "host": pcs.addr.ip().to_string(), "port": pcs.addr.port(), "unit_id": "1",
        "model_id": model_id, "point": point, "bit": bit,
    }))
    .unwrap()
}

async fn read(pcs: &Pcs, b: &SunspecBinding) -> Result<f64> {
    let mut session = channel(&b.host, pcs.addr.port(), None, None)?;
    read_on(&mut session, b).await
}

#[tokio::test]
async fn a_point_reads_through_its_scale_factor() -> Result<()> {
    // Arrange — W = 1234, W_SF = -1
    let pcs = spawn(40000).await?;
    pcs.set(at::W, 1234);
    pcs.set(at::W_SF, 0xFFFF);
    // Act
    let watts = read(&pcs, &binding(&pcs, 103, "W", None)).await?;
    // Assert
    assert!((watts - 123.4).abs() < 1e-9, "read {watts} W");
    Ok(())
}

#[tokio::test]
async fn a_map_that_starts_at_50000_is_found() -> Result<()> {
    // Arrange — nothing at 40000 or 0
    let pcs = spawn(50000).await?;
    pcs.set(at::SYS_FLT, 7);
    // Act
    let fault = read(&pcs, &binding(&pcs, 65534, "SysFlt", None)).await?;
    // Assert
    assert_eq!(fault, 7.0);
    Ok(())
}

#[tokio::test]
async fn a_not_implemented_point_is_an_error_not_a_reading() -> Result<()> {
    // Arrange — Hz reads 0xFFFF, SunSpec's uint16 "not implemented"
    let pcs = spawn(40000).await?;
    pcs.set(at::HZ, 0xFFFF);
    // Act
    let read = read(&pcs, &binding(&pcs, 103, "Hz", None)).await;
    // Assert
    let err = format!("{:#}", read.expect_err("published 65535 Hz"));
    assert!(err.contains("not implemented"), "{err}");
    Ok(())
}

#[tokio::test]
async fn a_bit_binding_reads_its_one_flag() -> Result<()> {
    // Arrange — CmdBits = 0b100 (bit 2 set), high word first
    let pcs = spawn(40000).await?;
    pcs.set(at::CMD_BITS + 1, 0b100);
    // Act
    let bit2 = read(&pcs, &binding(&pcs, 65534, "CmdBits", Some(2))).await?;
    let bit0 = read(&pcs, &binding(&pcs, 65534, "CmdBits", Some(0))).await?;
    // Assert
    assert_eq!((bit2, bit0), (1.0, 0.0));
    Ok(())
}
