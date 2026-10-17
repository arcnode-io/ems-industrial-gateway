//! SunSpec reads over an open Modbus session.

use crate::asyncapi::types::SunspecBinding;
use crate::modbus::codec::{ModbusDataType, WordOrder, decode_raw};
use crate::modbus::function_code::ReadFunction;
use crate::modbus::sunspec::apply_sunssf;
use crate::modbus::transport::read_registers;
use crate::sunspec::discover::{ModelMap, discover};
use crate::sunspec::models::{PointDef, PointType};
use crate::sunspec::resolve::{address, readable};
use anyhow::{Context, Result, anyhow, bail};
use rodbus::client::Channel;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// Discovered model maps, per device (host, port, unit id).
pub(crate) type Key = (String, u16, u8);
/// Shared by reads and writes; an entry goes on any failed request.
static MODELS: LazyLock<Mutex<HashMap<Key, ModelMap>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Read `b` on `channel`: discover the device's models (cached per device),
/// read the point's registers, refuse SunSpec's "not implemented" value,
/// then apply its scale factor or pick its bit.
pub async fn read_on(channel: &mut Channel, b: &SunspecBinding) -> Result<f64> {
    let def = readable(b).map_err(anyhow::Error::msg)?;
    let key = key(b)?;
    let read = read_point(channel, &key, b, def).await;
    // Reason: a failed read may mean the unit rebooted into firmware with
    // another layout; rediscover on the next read instead of trusting it.
    if read.is_err() {
        forget(&key);
    }
    read.with_context(|| format!("sunspec {} {}", b.model_id, b.point))
}

/// The device's model map, discovering it on first use.
pub(crate) async fn models(channel: &mut Channel, key: &Key) -> Result<ModelMap> {
    if let Some(models) = MODELS.lock().unwrap().get(key) {
        return Ok(models.clone());
    }
    let models = discover(channel, key.2).await?;
    MODELS.lock().unwrap().insert(key.clone(), models.clone());
    Ok(models)
}

/// Drop `key`'s model map, so its next request rediscovers.
pub(crate) fn forget(key: &Key) {
    MODELS.lock().unwrap().remove(key);
}

/// `read_on` without the cache eviction.
async fn read_point(
    channel: &mut Channel,
    key: &Key,
    b: &SunspecBinding,
    def: &PointDef,
) -> Result<f64> {
    let models = models(channel, key).await?;
    let (addr, sf_addr) = address(&models, b, def).map_err(anyhow::Error::msg)?;
    let words = read(channel, key.2, addr, def.kind).await?;
    let raw = decode_raw(&words, data_type(def.kind), WordOrder::HighLow);
    if let Some(bit) = b.bit {
        return Ok(f64::from((raw as u32 >> bit) & 1));
    }
    match sf_addr {
        Some(sf_addr) => {
            let sf = read(channel, key.2, sf_addr, PointType::Sunssf).await?;
            apply_sunssf(raw, sf[0]).map_err(anyhow::Error::msg)
        }
        None => Ok(raw),
    }
}

/// Read one point's registers, refusing its "not implemented" value.
pub(crate) async fn read(
    channel: &mut Channel,
    unit_id: u8,
    addr: u16,
    kind: PointType,
) -> Result<Vec<u16>> {
    let count = kind.register_count();
    let words = read_registers(channel, unit_id, addr, count, ReadFunction::Holding).await?;
    if words == not_implemented(kind) {
        bail!("register {addr} reads {words:04x?}, SunSpec's not implemented");
    }
    Ok(words)
}

/// The value SunSpec reserves for "not implemented", per type.
fn not_implemented(kind: PointType) -> Vec<u16> {
    match kind {
        PointType::Int16 | PointType::Sunssf => vec![0x8000],
        PointType::Uint16 | PointType::Enum16 => vec![0xFFFF],
        PointType::Int32 => vec![0x8000, 0x0000],
        PointType::Acc32 => vec![0x0000, 0x0000],
        PointType::Bitfield32 => vec![0xFFFF, 0xFFFF],
    }
}

/// The Modbus wire type a SunSpec point decodes as.
pub(crate) fn data_type(kind: PointType) -> ModbusDataType {
    match kind {
        PointType::Int16 | PointType::Sunssf => ModbusDataType::Int16,
        PointType::Uint16 | PointType::Enum16 => ModbusDataType::Uint16,
        PointType::Int32 => ModbusDataType::Int32,
        PointType::Acc32 | PointType::Bitfield32 => ModbusDataType::Uint32,
    }
}

/// Cache key for `b`'s device.
pub(crate) fn key(b: &SunspecBinding) -> Result<Key> {
    let unit_id = b
        .unit_id
        .parse()
        .map_err(|_| anyhow!("unit_id {} isn't a u8", b.unit_id))?;
    Ok((b.host.clone(), b.port, unit_id))
}
