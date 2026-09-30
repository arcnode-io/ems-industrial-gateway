//! DNP3 master client wrapping `dnp3::master::*`. Plain TCP + DNP3/TLS
//! (IEEE 1815 Annex E) branches share the same read pipeline; only the
//! channel-spawn step differs.
//!
//! One-shot read of a single analog or binary input at a given point_index;
//! the association and read itself live in `dnp3::master`.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::Dnp3TcpBinding;
use crate::config::GatewayCredentials;
use crate::dnp3::master::{PointKind, point_kind, read_with_channel};
use crate::dnp3::tls;
use anyhow::{Context, Result};
use dnp3::app::{ConnectStrategy, NullListener};
use dnp3::link::{EndpointAddress, LinkErrorMode};
use dnp3::master::MasterChannelConfig;
use dnp3::tcp::tls::spawn_master_tls_client;
use dnp3::tcp::{EndpointList, spawn_master_tcp_client};
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

/// Same retry curve as the other protocols.
const MAX_READ_ATTEMPTS: u32 = 5;
/// Local master address (arbitrary; outstation just needs to know who's talking).
const MASTER_ADDR: u16 = 1;

/// Full read pipeline for a DNP3 measurement.
///
/// `trust = Some(TlsMutual{..})` + `creds = Some(..)` → DNP3/TLS (CA-validated
/// mTLS, port 19999 standard). Else falls back to plain DNP3/TCP.
pub async fn read_measurement(
    b: &Dnp3TcpBinding,
    trust: Option<&DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<f64> {
    let kind = point_kind(&b.point_type).map_err(anyhow::Error::msg)?;
    let endpoint = format!("{}:{}", b.host, b.port);
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..MAX_READ_ATTEMPTS {
        let outcome = match (trust, creds) {
            (Some(DeviceTrust::TlsMutual { subject_name }), Some(creds)) => {
                try_read_tls(&endpoint, b.point_index, kind, subject_name, creds).await
            }
            _ => try_read_plain(&endpoint, b.point_index, kind).await,
        };
        match outcome {
            Ok(v) => return Ok(scaled(v, b)),
            Err(e) => {
                warn!(attempt, error = %e, "dnp3 read failed; retrying");
                last_err = Some(e);
                sleep(Duration::from_millis(500 * (1 << attempt))).await;
            }
        }
    }
    Err(last_err.unwrap()).context("dnp3 read exhausted retries")
}

/// Single plain-TCP read attempt.
async fn try_read_plain(endpoint: &str, point_index: u16, kind: PointKind) -> Result<f64> {
    let channel = spawn_master_tcp_client(
        LinkErrorMode::Close,
        MasterChannelConfig::new(EndpointAddress::try_new(MASTER_ADDR)?),
        EndpointList::single(endpoint.to_string()),
        ConnectStrategy::default(),
        NullListener::create(),
    );
    read_with_channel(channel, point_index, kind).await
}

/// Single DNP3/TLS read attempt. Builds `TlsClientConfig::full_pki` from the
/// gateway's mTLS material + the device's expected subject name.
async fn try_read_tls(
    endpoint: &str,
    point_index: u16,
    kind: PointKind,
    subject_name: &str,
    creds: &GatewayCredentials,
) -> Result<f64> {
    let tls_config = tls::build_tls_config(subject_name, creds)?;
    let channel = spawn_master_tls_client(
        LinkErrorMode::Close,
        MasterChannelConfig::new(EndpointAddress::try_new(MASTER_ADDR)?),
        EndpointList::single(endpoint.to_string()),
        ConnectStrategy::default(),
        NullListener::create(),
        tls_config,
    );
    read_with_channel(channel, point_index, kind).await
}

/// Raw point value to the measurement's unit (e.g. kV primary to volts).
pub(super) fn scaled(raw: f64, b: &Dnp3TcpBinding) -> f64 {
    raw * b.scale
}
