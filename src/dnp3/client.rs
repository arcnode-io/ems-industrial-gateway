//! DNP3 master client wrapping `dnp3::master::*`.
//!
//! Tier 1: one-shot read of a single AnalogInput at a given point_index.

use crate::asyncapi::types::Dnp3TcpBinding;
use anyhow::{Context, Result};
use dnp3::app::Variation;
use dnp3::app::measurement::AnalogInput;
use dnp3::app::{ConnectStrategy, MaybeAsync, NullListener, ResponseHeader};
use dnp3::link::{EndpointAddress, LinkErrorMode};
use dnp3::master::{
    AssociationConfig, AssociationHandler, AssociationInformation, Classes, EventClasses,
    HeaderInfo, MasterChannelConfig, ReadHandler, ReadRequest, ReadType,
};
use dnp3::tcp::{EndpointList, spawn_master_tcp_client};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

/// Same retry curve as the other protocols.
const MAX_READ_ATTEMPTS: u32 = 5;
/// Local master address (arbitrary; outstation just needs to know who's talking).
const MASTER_ADDR: u16 = 1;
/// Outstation address used by mock-dnp3-outstation.
const OUTSTATION_ADDR: u16 = 1024;

/// Full read pipeline for a DNP3 measurement.
pub async fn read_measurement(b: &Dnp3TcpBinding) -> Result<f64> {
    if b.point_type != "analog_input" {
        anyhow::bail!(
            "Tier 1 DNP3 only supports analog_input point_type, got {}",
            b.point_type
        );
    }
    let endpoint = format!("{}:{}", b.host, b.port);
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..MAX_READ_ATTEMPTS {
        match try_read(&endpoint, b.point_index).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                warn!(attempt, error = %e, "dnp3 read failed; retrying");
                last_err = Some(e);
                sleep(Duration::from_millis(500 * (1 << attempt))).await;
            }
        }
    }
    Err(last_err.unwrap()).context("dnp3 read exhausted retries")
}

/// Single attempt — connect a fresh master, add association, read one point.
async fn try_read(endpoint: &str, point_index: u16) -> Result<f64> {
    let mut channel = spawn_master_tcp_client(
        LinkErrorMode::Close,
        MasterChannelConfig::new(EndpointAddress::try_new(MASTER_ADDR)?),
        EndpointList::single(endpoint.to_string()),
        ConnectStrategy::default(),
        NullListener::create(),
    );
    let captured: Arc<Mutex<HashMap<u16, f64>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut association = channel
        .add_association(
            EndpointAddress::try_new(OUTSTATION_ADDR)?,
            association_config(),
            Box::new(Capturing::new(captured.clone())),
            Box::new(NopAssocHandler),
            Box::new(NopAssocInfo),
        )
        .await?;
    channel.enable().await?;

    // Wait for the integrity poll to complete before issuing the read.
    // (The startup poll runs on association add; a fresh read serializes after it.)
    let stop = u8::try_from(point_index).context("point_index must fit in u8 for Tier 1")?;
    association
        .read(ReadRequest::one_byte_range(
            Variation::Group30Var1,
            stop,
            stop,
        ))
        .await?;

    let map = captured.lock().expect("captured lock poisoned");
    map.get(&point_index)
        .copied()
        .with_context(|| format!("no AnalogInput at index {point_index} in response"))
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

/// ReadHandler that writes incoming AnalogInput values into a shared map.
struct Capturing {
    /// Captured `point_index -> value` from the most recent fragment.
    out: Arc<Mutex<HashMap<u16, f64>>>,
}
impl Capturing {
    /// Build a new handler wrapping the shared map.
    fn new(out: Arc<Mutex<HashMap<u16, f64>>>) -> Self {
        Self { out }
    }
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
        let mut map = self.out.lock().expect("out lock poisoned");
        for (ai, idx) in iter {
            map.insert(idx, ai.value);
        }
    }
}

/// AssociationHandler — defaults are fine for read-only.
struct NopAssocHandler;
impl AssociationHandler for NopAssocHandler {}

/// AssociationInformation — defaults are fine.
struct NopAssocInfo;
impl AssociationInformation for NopAssocInfo {}
