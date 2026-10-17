//! A sunspec binding checked against the shipped model definitions, and
//! resolved to registers once the device's model map is known.

use crate::asyncapi::types::SunspecBinding;
use crate::sunspec::discover::ModelMap;
use crate::sunspec::models::{PointDef, PointType, point};

/// Flag bits in a bitfield32.
const BITFIELD_BITS: u8 = 32;

/// The bound point, if it can be read: the model is shipped, the point is
/// in it, and a `bit` names one of a bitfield's 32.
pub fn readable(b: &SunspecBinding) -> Result<&'static PointDef, String> {
    let def = point(b.model_id, &b.point).ok_or_else(|| {
        format!(
            "no point {} in a shipped SunSpec model {}",
            b.point, b.model_id
        )
    })?;
    match (b.bit, def.kind) {
        (Some(bit), PointType::Bitfield32) if bit < BITFIELD_BITS => Ok(def),
        (Some(bit), PointType::Bitfield32) => Err(format!("bit {bit} is past a bitfield32")),
        (Some(_), kind) => Err(format!("{} is {kind:?}, not a bitfield", b.point)),
        (None, _) => Ok(def),
    }
}

/// The bound point, if it can also be written: readable, writable, and a
/// bitfield is only written one named bit at a time.
pub fn writable(b: &SunspecBinding) -> Result<&'static PointDef, String> {
    let def = readable(b)?;
    if !def.writable {
        return Err(format!("{} in model {} is read-only", b.point, b.model_id));
    }
    // Reason: writing a whole bitfield would set every flag the gateway
    // has no binding for; one named bit leaves the others as they are.
    if def.kind == PointType::Bitfield32 && b.bit.is_none() {
        return Err(format!(
            "{} is a bitfield; a command names one bit",
            b.point
        ));
    }
    Ok(def)
}

/// Register address of `def` (and of its scale factor, if it has one) on a
/// device whose models sit at `models`.
pub fn address(
    models: &ModelMap,
    b: &SunspecBinding,
    def: &PointDef,
) -> Result<(u16, Option<u16>), String> {
    let start = *models
        .get(&b.model_id)
        .ok_or_else(|| format!("device serves no SunSpec model {}", b.model_id))?;
    let sf = def
        .sf
        .map(|id| point(b.model_id, id).expect("model tables name their own sf points"));
    Ok((start + def.offset, sf.map(|sf| start + sf.offset)))
}
