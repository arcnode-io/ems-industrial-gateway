//! Modbus TCP / Modbus Security (TLS+Role) client — connection + read/write
//! pipelines. Pure decode/encode lives in `modbus::codec`; re-exported here
//! so existing call sites (`modbus::client::{WordOrder, decode_int32, ...}`)
//! keep working unchanged.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::ModbusTcpBinding;
use crate::config::GatewayCredentials;
use crate::modbus::codec::{decode_raw, encode_raw};
use crate::modbus::tls;
use anyhow::{Context, Result};
use rodbus::client::{
    Channel, HostAddr, RequestParam, spawn_tcp_client_task, spawn_tls_client_task,
};
use rodbus::{AddressRange, UnitId};
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

pub use crate::modbus::codec::{
    ModbusDataType, WordOrder, apply_scale_offset, decode_int32, encode_int32, to_raw,
};

/// Attempts to retry on transient "no connection to server" — rodbus channels
/// reconnect in the background and the first read can race with the initial
/// TCP / TLS handshake.
const MAX_READ_ATTEMPTS: u32 = 5;

/// Full read pipeline for a Modbus measurement: connect → read
/// `data_type.register_count()` holding registers → decode per data type +
/// word order → apply scale/offset.
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
    let count = b.data_type.register_count();
    let words = match (trust, creds) {
        (Some(DeviceTrust::TlsMutual { subject_name }), Some(creds)) => {
            read_holding_tls(
                &b.host,
                b.port,
                unit_id,
                b.address,
                count,
                subject_name,
                creds,
            )
            .await?
        }
        _ => read_holding(&b.host, b.port, unit_id, b.address, count).await?,
    };
    let raw = decode_raw(&words, b.data_type, b.word_order);
    Ok(apply_scale_offset(raw, b.scale, b.offset))
}

/// Full write pipeline for a Modbus command: engineering value → raw →
/// encode per data type + word order → connect → write holding registers
/// (function code 16).
///
/// Same trust/creds dialing rules as `read_measurement` — Modbus Security
/// when the device requires mTLS and the gateway has creds, plain TCP
/// otherwise.
pub async fn write_setpoint(
    b: &ModbusTcpBinding,
    value: f64,
    trust: Option<&DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<()> {
    let unit_id: u8 = b
        .unit_id
        .parse()
        .context("unit_id must parse to u8 for Modbus")?;
    let raw = to_raw(value, b.scale, b.offset);
    let words = encode_raw(raw, b.data_type, b.word_order);
    match (trust, creds) {
        (Some(DeviceTrust::TlsMutual { subject_name }), Some(creds)) => {
            write_holding_tls(
                &b.host,
                b.port,
                unit_id,
                b.address,
                &words,
                subject_name,
                creds,
            )
            .await
        }
        _ => write_holding(&b.host, b.port, unit_id, b.address, &words).await,
    }
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

/// Connect over plain TCP and write `words` starting at `addr` (function
/// code 16, write multiple registers).
pub async fn write_holding(
    host: &str,
    port: u16,
    unit_id: u8,
    addr: u16,
    words: &[u16],
) -> Result<()> {
    let channel = spawn_tcp_client_task(
        HostAddr::dns(host.to_string(), port),
        1,
        rodbus::default_retry_strategy(),
        rodbus::DecodeLevel::default(),
        None,
    );
    write_with_channel(channel, unit_id, addr, words).await
}

/// Connect over Modbus Security (TLS) and write `words` starting at `addr`.
/// Same subject-name-pinned `TlsClientConfig::full_pki` as `read_holding_tls`.
#[allow(clippy::too_many_arguments)]
pub async fn write_holding_tls(
    host: &str,
    port: u16,
    unit_id: u8,
    addr: u16,
    words: &[u16],
    subject_name: &str,
    creds: &GatewayCredentials,
) -> Result<()> {
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
    write_with_channel(channel, unit_id, addr, words).await
}

/// Enable + write loop. Shared by plain + TLS paths — only the channel
/// source differs. Retries on transient errors per `MAX_READ_ATTEMPTS`
/// (same budget as reads; the failure mode — racing the initial
/// TCP/TLS handshake — is identical).
async fn write_with_channel(
    mut channel: Channel,
    unit_id: u8,
    addr: u16,
    words: &[u16],
) -> Result<()> {
    channel.enable().await.context("modbus channel enable")?;
    let request = rodbus::client::WriteMultiple::from(addr, words.to_vec())
        .map_err(|e| anyhow::anyhow!("invalid modbus write request: {e}"))?;
    let param = RequestParam::new(UnitId::new(unit_id), Duration::from_secs(5));

    let mut last_err = None;
    for attempt in 0..MAX_READ_ATTEMPTS {
        match channel
            .write_multiple_registers(param, request.clone())
            .await
        {
            Ok(_) => return Ok(()),
            Err(e) => {
                warn!(attempt, error = %e, "modbus write_multiple_registers failed; retrying");
                last_err = Some(e);
                sleep(Duration::from_millis(500 * (1 << attempt))).await;
            }
        }
    }
    Err(last_err.unwrap()).context("modbus write_multiple_registers exhausted retries")
}
