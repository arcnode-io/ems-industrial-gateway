//! Modbus TCP / Modbus Security (TLS+Role) client + decode helpers.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::ModbusTcpBinding;
use crate::config::GatewayCredentials;
use crate::modbus::tls;
use anyhow::{Context, Result};
use rodbus::client::{
    Channel, HostAddr, RequestParam, spawn_tcp_client_task, spawn_tls_client_task,
};
use rodbus::{AddressRange, UnitId};
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

/// Attempts to retry on transient "no connection to server" — rodbus channels
/// reconnect in the background and the first read can race with the initial
/// TCP / TLS handshake.
const MAX_READ_ATTEMPTS: u32 = 5;

/// Full read pipeline for a Modbus measurement: connect → read 2 holding
/// registers → decode int32 high_low → apply scale/offset.
///
/// `trust` carries the device's `x-device-trust` block. `creds` is the
/// gateway's global mTLS material. `Some(TlsMutual{..})` + `Some(creds)`
/// dials Modbus Security (CA-validated mTLS + Role extension authz, per
/// Modbus Security spec). Anything else falls back to plain Modbus/TCP.
///
/// The gateway's client cert (at `creds.cert_path`) must carry the Modbus
/// Role extension (OID 1.3.6.1.4.1.50316.802.1) — the CA / issuance flow
/// owns that, not this code.
pub async fn read_measurement(
    b: &ModbusTcpBinding,
    trust: Option<&DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<f64> {
    let unit_id: u8 = b
        .unit_id
        .parse()
        .context("unit_id must parse to u8 for Modbus")?;
    let words = match (trust, creds) {
        (Some(DeviceTrust::TlsMutual { subject_name }), Some(creds)) => {
            read_holding_tls(&b.host, b.port, unit_id, b.address, 2, subject_name, creds).await?
        }
        _ => read_holding(&b.host, b.port, unit_id, b.address, 2).await?,
    };
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

/// Connect over plain TCP and read `count` holding registers starting at `addr`.
pub async fn read_holding(
    host: &str,
    port: u16,
    unit_id: u8,
    addr: u16,
    count: u16,
) -> Result<Vec<u16>> {
    let channel = spawn_tcp_client_task(
        HostAddr::dns(host.to_string(), port),
        1,
        rodbus::default_retry_strategy(),
        rodbus::DecodeLevel::default(),
        None,
    );
    read_with_channel(channel, unit_id, addr, count).await
}

/// Connect over Modbus Security (TLS) and read `count` holding registers.
/// Subject name + creds drive `TlsClientConfig::full_pki` — the device's cert
/// must chain to the configured CA AND present a SAN/CN matching `subject_name`.
#[allow(clippy::too_many_arguments)]
pub async fn read_holding_tls(
    host: &str,
    port: u16,
    unit_id: u8,
    addr: u16,
    count: u16,
    subject_name: &str,
    creds: &GatewayCredentials,
) -> Result<Vec<u16>> {
    let tls_config = tls::build_tls_config(
        subject_name,
        &creds.ca_bundle_path,
        &creds.cert_path,
        &creds.key_path,
    )?;
    let channel = spawn_tls_client_task(
        HostAddr::dns(host.to_string(), port),
        1,
        rodbus::default_retry_strategy(),
        tls_config,
        rodbus::DecodeLevel::default(),
        None,
    );
    read_with_channel(channel, unit_id, addr, count).await
}

/// Enable + read loop. Shared by plain + TLS paths — only the channel source
/// differs. Retries on transient errors per `MAX_READ_ATTEMPTS`.
async fn read_with_channel(
    mut channel: Channel,
    unit_id: u8,
    addr: u16,
    count: u16,
) -> Result<Vec<u16>> {
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
