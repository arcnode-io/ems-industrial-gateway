//! SNMP v2c client wrapping `csnmp::Snmp2cClient`.

use crate::asyncapi::types::SnmpBinding;
use anyhow::{Context, Result};
use csnmp::{ObjectIdentifier, ObjectValue, Snmp2cClient};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::lookup_host;
use tokio::time::sleep;
use tracing::warn;

/// Full read pipeline for an SNMP measurement: resolve → GET → cast to f64.
pub async fn read_measurement(b: &SnmpBinding) -> Result<f64> {
    let raw = read_integer(&b.host, b.port, &b.oid).await?;
    Ok(raw as f64)
}

/// Default community for SNMP v2c reads. Industrial gear typically allows
/// "public" for read-only access. Override via cfg in the future when
/// per-device community strings are needed.
const COMMUNITY: &[u8] = b"public";
/// Retry policy mirrors Modbus — first request can race with UDP arrival/processing.
const MAX_READ_ATTEMPTS: u32 = 5;

/// Resolve `host:port` and SNMP GET on `oid`. Returns the integer value or
/// errors if the OID isn't an integer-typed object.
pub async fn read_integer(host: &str, port: u16, oid: &str) -> Result<i64> {
    let socket: SocketAddr = lookup_host((host, port))
        .await
        .context("resolve snmp host:port")?
        .next()
        .context("no addrs from snmp host:port resolution")?;
    let parsed_oid: ObjectIdentifier = oid.parse().context("parse OID dotted-numeric")?;

    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..MAX_READ_ATTEMPTS {
        match try_get(socket, COMMUNITY.to_vec(), parsed_oid).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                warn!(attempt, error = %e, "snmp get failed; retrying");
                last_err = Some(e);
                sleep(Duration::from_millis(500 * (1 << attempt))).await;
            }
        }
    }
    Err(last_err.unwrap()).context("snmp get exhausted retries")
}

/// Single attempt — build a client, send GET, decode the integer.
async fn try_get(socket: SocketAddr, community: Vec<u8>, oid: ObjectIdentifier) -> Result<i64> {
    let client = Snmp2cClient::new(socket, community, None, None, 0)
        .await
        .context("build Snmp2cClient")?;
    let value = client.get(oid).await.context("snmp get")?;
    match value {
        ObjectValue::Integer(i) => Ok(i64::from(i)),
        ObjectValue::Counter32(c) => Ok(i64::from(c)),
        ObjectValue::Unsigned32(u) => Ok(i64::from(u)),
        ObjectValue::Counter64(c) => Ok(c as i64),
        other => anyhow::bail!("expected integer-shaped SNMP value, got {other:?}"),
    }
}
