//! Modbus TCP / Modbus Security (TLS+Role) client — read/write pipelines.
//! Pure decode/encode lives in `modbus::codec`, channels and register I/O in
//! `modbus::transport`. Codec items are re-exported here so existing call
//! sites (`modbus::client::{WordOrder, decode_int32, ...}`) keep working.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::ModbusTcpBinding;
use crate::config::GatewayCredentials;
use crate::modbus::codec::{
    ReadFunction, WriteFunction, decode_raw, encode_raw, read_function, write_function,
};
use crate::modbus::sunspec::apply_sunssf;
use crate::modbus::transport::{read_registers, tcp_channel, tls_channel, write_registers};
use anyhow::{Context, Result};
use rodbus::client::Channel;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

pub use crate::modbus::codec::{
    ModbusDataType, WordOrder, apply_scale_offset, decode_int32, encode_int32, to_raw,
};

/// Full read pipeline for a Modbus measurement: connect → read
/// `data_type.register_count()` registers with the binding's function code
/// (FC3 holding, FC4 input) → decode per data type + word order → apply the
/// SunSpec scale factor if the binding names one → apply scale/offset.
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
    read_on(&mut channel(b, trust, creds)?, b).await
}

/// `read_measurement` over an open session, so a device's readings can
/// share one (see `poller`).
pub async fn read_on(channel: &mut Channel, b: &ModbusTcpBinding) -> Result<f64> {
    let function = read_function(b.function_code).map_err(anyhow::Error::msg)?;
    let unit_id = unit_id(b)?;
    let count = b.data_type.register_count();
    let words = read_registers(channel, unit_id, b.address, count, function).await?;
    let mut raw = decode_raw(&words, b.data_type, b.word_order);
    // SunSpec: the exponent lives in its own register, read on the same session.
    if let Some(sf_address) = b.scale_factor_address {
        let sf = read_registers(channel, unit_id, sf_address, 1, function).await?;
        raw = apply_sunssf(raw, sf[0]).map_err(anyhow::Error::msg)?;
    }
    Ok(apply_scale_offset(raw, b.scale, b.offset))
}

/// Full write pipeline for a Modbus command: engineering value → raw →
/// encode per data type + word order → connect → write with the binding's
/// function code (FC6 single, FC16 multiple).
///
/// Same trust/creds dialing rules as `read_measurement`.
pub async fn write_setpoint(
    b: &ModbusTcpBinding,
    value: f64,
    trust: Option<&DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<()> {
    let function = write_function(b.function_code, b.data_type).map_err(anyhow::Error::msg)?;
    let words = encode_raw(to_raw(value, b.scale, b.offset), b.data_type, b.word_order);
    let channel = write_channel(b, trust, creds)?;
    write_registers(channel, unit_id(b)?, b.address, &words, function).await
}

/// One open session per device for writes, kept across writes.
///
/// Reason: a session per write connected after its first request went out,
/// so every write failed, backed off 500 ms and landed late. The envelope
/// writes each rack every tick; late writes skewed what the battery
/// delivered.
fn write_channel(
    b: &ModbusTcpBinding,
    trust: Option<&DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<Channel> {
    type Key = (String, u16, bool);
    static SESSIONS: LazyLock<Mutex<HashMap<Key, Channel>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let secure = matches!(
        (trust, creds),
        (Some(DeviceTrust::TlsMutual { .. }), Some(_))
    );
    let key = (b.host.clone(), b.port, secure);
    if let Some(ch) = SESSIONS.lock().unwrap().get(&key) {
        return Ok(ch.clone());
    }
    let ch = channel(b, trust, creds)?;
    Ok(SESSIONS.lock().unwrap().entry(key).or_insert(ch).clone())
}

/// Plain TCP read of `count` holding registers (FC3) at `addr`.
pub async fn read_holding(
    host: &str,
    port: u16,
    unit_id: u8,
    addr: u16,
    count: u16,
) -> Result<Vec<u16>> {
    let mut channel = tcp_channel(host, port);
    read_registers(&mut channel, unit_id, addr, count, ReadFunction::Holding).await
}

/// Plain TCP write of `words` at `addr` (FC16, write multiple registers).
pub async fn write_holding(
    host: &str,
    port: u16,
    unit_id: u8,
    addr: u16,
    words: &[u16],
) -> Result<()> {
    write_registers(
        tcp_channel(host, port),
        unit_id,
        addr,
        words,
        WriteFunction::Multiple,
    )
    .await
}

/// A session to `b`'s host: Modbus Security when the device requires mTLS
/// and the gateway has creds, plain TCP otherwise.
pub fn channel(
    b: &ModbusTcpBinding,
    trust: Option<&DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<Channel> {
    match (trust, creds) {
        (Some(DeviceTrust::TlsMutual { subject_name }), Some(creds)) => {
            tls_channel(&b.host, b.port, subject_name, creds)
        }
        _ => Ok(tcp_channel(&b.host, b.port)),
    }
}

/// The DTM stores unit_id as a string; Modbus wants a u8.
fn unit_id(b: &ModbusTcpBinding) -> Result<u8> {
    b.unit_id
        .parse()
        .context("unit_id must parse to u8 for Modbus")
}
