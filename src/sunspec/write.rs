//! SunSpec writes on the device's shared write session: scaled setpoints,
//! and bitfield words for the heartbeat and bit commands.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::SunspecBinding;
use crate::config::GatewayCredentials;
use crate::modbus::client::write_channel;
use crate::modbus::codec::{WordOrder, decode_uint32, encode_raw, encode_uint32};
use crate::modbus::function_code::WriteFunction;
use crate::modbus::sunspec::apply_sunssf;
use crate::modbus::transport::write_registers;
use crate::sunspec::bits::{BitAction, apply, slot, word_key};
use crate::sunspec::client::{Key, data_type, forget, key, models, read};
use crate::sunspec::models::{PointDef, PointType};
use crate::sunspec::resolve::{address, readable, writable};
use anyhow::{Context, Result, anyhow};
use rodbus::client::Channel;

/// Write `value` (in the point's engineering units) to `b`: scale it by the
/// point's sunssf, encode it per the point's type and write it with FC16.
pub async fn write_setpoint(
    b: &SunspecBinding,
    value: f64,
    trust: Option<&DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<()> {
    let key = key(b)?;
    let written = write(b, value, &key, trust, creds).await;
    if written.is_err() {
        forget(&key);
    }
    written.with_context(|| format!("sunspec {} {}", b.model_id, b.point))
}

/// `write_setpoint` without the cache eviction.
async fn write(
    b: &SunspecBinding,
    value: f64,
    key: &Key,
    trust: Option<&DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<()> {
    let def = writable(b).map_err(anyhow::Error::msg)?;
    if b.bit.is_some() {
        return Err(anyhow!(
            "{} bit commands aren't written as setpoints",
            b.point
        ));
    }
    let mut channel = write_channel(&b.host, b.port, trust, creds)?;
    let models = models(&mut channel, key).await?;
    let (addr, sf_addr) = address(&models, b, def).map_err(anyhow::Error::msg)?;
    // Reason: the register counts units of 10^sf, so 250 kW at sf = 1 is 25_000.
    let scale = match sf_addr {
        Some(sf_addr) => {
            let sf = read(&mut channel, key.2, sf_addr, PointType::Sunssf).await?;
            apply_sunssf(1.0, sf[0]).map_err(anyhow::Error::msg)?
        }
        None => 1.0,
    };
    let words = encode_raw(value / scale, data_type(def.kind), WordOrder::HighLow)
        .map_err(anyhow::Error::msg)?;
    write_registers(channel, key.2, addr, &words, WriteFunction::Multiple).await
}

/// Apply a bit command to `b`'s word: write the result, then keep asserting
/// it (a trigger's bit excepted). Fails when no heartbeat feeds the word,
/// since a PCS without one has already dropped out.
pub async fn write_bit(
    b: &SunspecBinding,
    action: &BitAction,
    trust: Option<&DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<()> {
    let bit = b.bit.ok_or_else(|| anyhow!("{} names no bit", b.point))?;
    let slot = slot(&word_key(b));
    let mut word = slot.lock().await;
    let (now, keep) = apply(*word, bit, action)?;
    write_word(b, now, trust, creds).await?;
    *word = Some(keep);
    Ok(())
}

/// Read `b`'s whole bitfield word.
pub async fn read_word(
    b: &SunspecBinding,
    trust: Option<&DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<u32> {
    let key = key(b)?;
    let mut channel = write_channel(&b.host, b.port, trust, creds)?;
    let (addr, def) = word_address(&mut channel, &key, b).await?;
    let words = read(&mut channel, key.2, addr, def.kind).await?;
    Ok(decode_uint32(&words, WordOrder::HighLow))
}

/// Write `word` to `b`'s whole bitfield.
pub async fn write_word(
    b: &SunspecBinding,
    word: u32,
    trust: Option<&DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<()> {
    let key = key(b)?;
    let mut channel = write_channel(&b.host, b.port, trust, creds)?;
    let (addr, _) = word_address(&mut channel, &key, b).await?;
    let words = encode_uint32(word, WordOrder::HighLow);
    let written = write_registers(channel, key.2, addr, &words, WriteFunction::Multiple).await;
    if written.is_err() {
        forget(&key);
    }
    written.with_context(|| format!("sunspec {} {}", b.model_id, b.point))
}

/// Address of `b`'s bitfield word on its device.
async fn word_address(
    channel: &mut Channel,
    key: &Key,
    b: &SunspecBinding,
) -> Result<(u16, &'static PointDef)> {
    let def = readable(b).map_err(anyhow::Error::msg)?;
    if def.kind != PointType::Bitfield32 {
        return Err(anyhow!("{} isn't a bitfield", b.point));
    }
    let models = models(channel, key).await?;
    let (addr, _) = address(&models, b, def).map_err(anyhow::Error::msg)?;
    Ok((addr, def))
}
