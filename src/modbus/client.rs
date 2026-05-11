//! Modbus TCP client + decode helpers.

use anyhow::{Context, Result};
use rodbus::client::{spawn_tcp_client_task, HostAddr, RequestParam};
use rodbus::{AddressRange, UnitId};
use std::time::Duration;

/// Word order for multi-register integer decoding.
#[derive(Debug, Clone, Copy)]
pub enum WordOrder {
    /// High word first (AB CD).
    HighLow,
    /// Low word first (CD AB).
    LowHigh,
}

/// Connect over TCP and read `count` holding registers starting at `addr`.
pub async fn read_holding(
    host: &str,
    port: u16,
    unit_id: u8,
    addr: u16,
    count: u16,
) -> Result<Vec<u16>> {
    let mut channel = spawn_tcp_client_task(
        HostAddr::dns(host.to_string(), port),
        1,
        rodbus::default_retry_strategy(),
        rodbus::DecodeLevel::default(),
        None,
    );
    channel.enable().await.context("modbus channel enable")?;

    let range = AddressRange::try_from(addr, count)
        .map_err(|e| anyhow::anyhow!("invalid modbus address range: {e}"))?;
    let result = channel
        .read_holding_registers(
            RequestParam::new(UnitId::new(unit_id), Duration::from_secs(5)),
            range,
        )
        .await
        .context("modbus read_holding_registers")?;
    Ok(result.iter().map(|r| r.value).collect())
}

/// Decode two consecutive u16 holding registers as a signed 32-bit integer.
pub fn decode_int32(words: &[u16], order: WordOrder) -> i32 {
    let (high, low) = match order {
        WordOrder::HighLow => (words[0], words[1]),
        WordOrder::LowHigh => (words[1], words[0]),
    };
    (((high as u32) << 16) | (low as u32)) as i32
}

/// Apply Modbus scale + offset to a raw integer reading.
pub fn apply_scale_offset(raw: i32, scale: f64, offset: f64) -> f64 {
    raw as f64 * scale + offset
}

