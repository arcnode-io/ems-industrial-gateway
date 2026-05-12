//! Modbus TCP client + decode helpers.

use crate::asyncapi::types::ModbusTcpBinding;
use anyhow::{Context, Result};
use rodbus::client::{HostAddr, RequestParam, spawn_tcp_client_task};
use rodbus::{AddressRange, UnitId};
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

/// Attempts to retry on transient "no connection to server" — rodbus channels
/// reconnect in the background and the first read can race with the initial
/// TCP handshake.
const MAX_READ_ATTEMPTS: u32 = 5;

/// Full read pipeline for a Modbus measurement: connect → read 2 holding
/// registers → decode int32 high_low → apply scale/offset.
pub async fn read_measurement(b: &ModbusTcpBinding) -> Result<f64> {
    let unit_id: u8 = b
        .unit_id
        .parse()
        .context("unit_id must parse to u8 for Modbus")?;
    let words = read_holding(&b.host, b.port, unit_id, b.address, 2).await?;
    let raw = decode_int32(&words, WordOrder::HighLow);
    Ok(apply_scale_offset(raw, b.scale, b.offset))
}

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
    let param = RequestParam::new(UnitId::new(unit_id), Duration::from_secs(5));

    let mut last_err = None;
    for attempt in 0..MAX_READ_ATTEMPTS {
        match channel.read_holding_registers(param, range).await {
            Ok(result) => return Ok(result.iter().map(|r| r.value).collect()),
            Err(e) => {
                warn!(attempt, error = %e, "modbus read_holding_registers failed; retrying");
                last_err = Some(e);
                sleep(Duration::from_millis(500 * (1 << attempt))).await;
            }
        }
    }
    Err(last_err.unwrap()).context("modbus read_holding_registers exhausted retries")
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
