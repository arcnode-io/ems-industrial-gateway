//! Model discovery: find the `SunS` marker, then walk the model chain
//! (each model an ID/L header, then L registers) to the 0xFFFF end marker.

use crate::modbus::function_code::ReadFunction;
use crate::modbus::transport::read_registers;
use anyhow::{Result, bail};
use rodbus::RequestError;
use rodbus::client::Channel;
use std::collections::HashMap;

/// Model id → address of its first point (just past its ID/L header).
pub type ModelMap = HashMap<u16, u16>;

/// `SunS`, the marker a SunSpec map starts with.
const MARKER: [u16; 2] = [0x5375, 0x6e53];

/// Where SunSpec says to look for the marker, in order.
const BASES: [u16; 3] = [40000, 0, 50000];

/// Model id that ends the chain.
const END: u16 = 0xFFFF;

/// Discover the models `unit_id` serves on `channel`.
pub async fn discover(channel: &mut Channel, unit_id: u8) -> Result<ModelMap> {
    for base in BASES {
        match read_registers(channel, unit_id, base, 2, ReadFunction::Holding).await {
            Ok(words) if words == MARKER => return walk(channel, unit_id, base + 2).await,
            Ok(_) => {}
            // Reason: an exception is the device saying it has no register
            // there, so try the next base; anything else is a dead link.
            Err(e) if e.downcast_ref::<RequestError>().is_some_and(is_exception) => {}
            Err(e) => return Err(e.context("SunSpec discovery")),
        }
    }
    bail!("no SunSpec marker at {BASES:?}")
}

/// Walk the chain from the first model header at `addr`.
async fn walk(channel: &mut Channel, unit_id: u8, mut addr: u16) -> Result<ModelMap> {
    let mut models = ModelMap::new();
    loop {
        let header = read_registers(channel, unit_id, addr, 2, ReadFunction::Holding).await?;
        let (id, len) = (header[0], header[1]);
        if id == END {
            return Ok(models);
        }
        let start = addr + 2;
        models.insert(id, start);
        // Reason: a corrupt length could wrap the address space and walk
        // the chain forever.
        let Some(next) = start.checked_add(len) else {
            bail!("SunSpec model {id} at {addr} runs past register 65535");
        };
        addr = next;
    }
}

/// The device answered with a Modbus exception.
fn is_exception(e: &RequestError) -> bool {
    matches!(e, RequestError::Exception(_))
}
