//! DNP3 master read: add an association, read one point, capture its value.

use anyhow::{Context, Result};
use dnp3::app::Variation;
use dnp3::app::measurement::{AnalogInput, BinaryInput};
use dnp3::app::{MaybeAsync, ResponseHeader};
use dnp3::link::EndpointAddress;
use dnp3::master::{
    AssociationConfig, AssociationHandler, AssociationInformation, Classes, EventClasses,
    HeaderInfo, MasterChannel, ReadHandler, ReadRequest, ReadType,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Outstation address used by mock-dnp3-outstation.
const OUTSTATION_ADDR: u16 = 1024;

/// Which static object group a measurement reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointKind {
    /// Group 30, read as a float.
    AnalogInput,
    /// Group 1, read as 1.0 (set) or 0.0 (clear).
    BinaryInput,
}

/// A binding's `point_type` as a readable point kind. Outputs and counters
/// aren't read yet.
pub fn point_kind(point_type: &str) -> Result<PointKind, String> {
    match point_type {
        "analog_input" => Ok(PointKind::AnalogInput),
        "binary_input" => Ok(PointKind::BinaryInput),
        other => Err(format!(
            "DNP3 point_type {other} is not read (analog_input or binary_input)"
        )),
    }
}

/// Add the association, enable, read `point_index` of `kind`, return it.
///
/// Reason for variation 0: it asks for the outstation's configured static
/// variation. Forcing Group30Var1 (32-bit integer) truncated a relay's
/// 13.8 kV to 13 kV.
pub async fn read_with_channel(
    mut channel: MasterChannel,
    point_index: u16,
    kind: PointKind,
) -> Result<f64> {
    let captured: Arc<Mutex<HashMap<u16, f64>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut association = channel
        .add_association(
            EndpointAddress::try_new(OUTSTATION_ADDR)?,
            association_config(),
            Box::new(Capturing {
                kind,
                out: captured.clone(),
            }),
            Box::new(NopAssocHandler),
            Box::new(NopAssocInfo),
        )
        .await?;
    channel.enable().await?;

    let stop = u8::try_from(point_index).context("point_index must fit in u8")?;
    let variation = match kind {
        PointKind::AnalogInput => Variation::Group30Var0,
        PointKind::BinaryInput => Variation::Group1Var0,
    };
    association
        .read(ReadRequest::one_byte_range(variation, stop, stop))
        .await?;

    let map = captured.lock().expect("captured lock poisoned");
    map.get(&point_index)
        .copied()
        .with_context(|| format!("no {kind:?} at index {point_index} in response"))
}

/// Minimal association config — disable unsolicited, do a startup integrity
/// poll of all classes so the outstation's static values land in the cache.
fn association_config() -> AssociationConfig {
    AssociationConfig::new(
        EventClasses::none(),
        EventClasses::none(),
        Classes::all(),
        EventClasses::none(),
    )
}

/// ReadHandler that writes incoming values of one point kind into a shared
/// map; values of the other kind (from the startup integrity poll) are ignored.
struct Capturing {
    /// Which kind this read is for.
    kind: PointKind,
    /// Captured `point_index -> value` from the most recent fragment.
    out: Arc<Mutex<HashMap<u16, f64>>>,
}

impl ReadHandler for Capturing {
    fn begin_fragment(&mut self, _r: ReadType, _h: ResponseHeader) -> MaybeAsync<()> {
        MaybeAsync::ready(())
    }
    fn end_fragment(&mut self, _r: ReadType, _h: ResponseHeader) -> MaybeAsync<()> {
        MaybeAsync::ready(())
    }
    fn handle_analog_input(
        &mut self,
        _info: HeaderInfo,
        iter: &mut dyn Iterator<Item = (AnalogInput, u16)>,
    ) {
        if self.kind == PointKind::AnalogInput {
            let mut map = self.out.lock().expect("out lock poisoned");
            map.extend(iter.map(|(ai, idx)| (idx, ai.value)));
        }
    }
    fn handle_binary_input(
        &mut self,
        _info: HeaderInfo,
        iter: &mut dyn Iterator<Item = (BinaryInput, u16)>,
    ) {
        if self.kind == PointKind::BinaryInput {
            let mut map = self.out.lock().expect("out lock poisoned");
            map.extend(iter.map(|(bi, idx)| (idx, if bi.value { 1.0 } else { 0.0 })));
        }
    }
}

/// AssociationHandler — defaults are fine for read-only.
struct NopAssocHandler;
impl AssociationHandler for NopAssocHandler {}

/// AssociationInformation — defaults are fine.
struct NopAssocInfo;
impl AssociationInformation for NopAssocInfo {}
