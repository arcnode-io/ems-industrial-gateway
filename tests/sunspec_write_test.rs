//! e2e: a sunspec setpoint is written in the point's own units: the gateway
//! reads its scale factor and writes the int32 the PCS expects, and refuses
//! a value that doesn't fit rather than saturating it.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::asyncapi::types::SunspecBinding;
use ems_industrial_gateway::sunspec::write::write_setpoint;
use fixtures::sunspec_pcs::{Pcs, at, spawn};
use serde_json::json;

fn binding(pcs: &Pcs, point: &str) -> SunspecBinding {
    serde_json::from_value(json!({
        "host": pcs.addr.ip().to_string(), "port": pcs.addr.port(), "unit_id": "1",
        "model_id": 65534, "point": point,
    }))
    .unwrap()
}

#[tokio::test]
async fn real_power_is_written_as_an_int32_in_the_scale_factor_s_units() -> Result<()> {
    // Arrange — PWR_SF = 1: the register counts tens of watts
    let pcs = spawn(40000).await?;
    pcs.set(at::PWR_SF, 1);
    // Act — 250 kW discharge
    write_setpoint(&binding(&pcs, "CmdRealPwr"), 250_000.0, None, None).await?;
    // Assert — 25_000 = 0x0000_61A8, high word first
    let writes = pcs.writes_at(at::CMD_REAL_PWR);
    assert_eq!(
        writes.last().map(|w| w.words.clone()),
        Some(vec![0x0000, 0x61A8])
    );
    Ok(())
}

#[tokio::test]
async fn negative_reactive_power_is_a_twos_complement_int32() -> Result<()> {
    // Arrange — PWR_SF = 0
    let pcs = spawn(40000).await?;
    // Act — absorb 1 kvar
    write_setpoint(&binding(&pcs, "CmdReactivePwr"), -1_000.0, None, None).await?;
    // Assert — -1000 = 0xFFFF_FC18
    let writes = pcs.writes_at(at::CMD_REACTIVE_PWR);
    assert_eq!(
        writes.last().map(|w| w.words.clone()),
        Some(vec![0xFFFF, 0xFC18])
    );
    Ok(())
}

#[tokio::test]
async fn a_setpoint_that_doesnt_fit_is_refused_and_never_written() -> Result<()> {
    // Arrange — PWR_SF = -3: the register counts milliwatts
    let pcs = spawn(40000).await?;
    pcs.set(at::PWR_SF, (-3i16) as u16);
    // Act — 5 MW is 5e9 mW, past an int32
    let result = write_setpoint(&binding(&pcs, "CmdRealPwr"), 5e6, None, None).await;
    // Assert
    assert!(result.is_err());
    assert!(pcs.writes_at(at::CMD_REAL_PWR).is_empty());
    Ok(())
}
