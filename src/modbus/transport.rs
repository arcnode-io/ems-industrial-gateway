//! Modbus wire plumbing: open a plain or Modbus Security (TLS) channel, then
//! read or write registers on it with the binding's function code.

use crate::config::GatewayCredentials;
use crate::modbus::codec::{ReadFunction, WriteFunction};
use crate::modbus::tls;
use anyhow::{Context, Result};
use rodbus::client::{
    Channel, HostAddr, RequestParam, WriteMultiple, spawn_tcp_client_task, spawn_tls_client_task,
};
use rodbus::{AddressRange, Indexed, RequestError, UnitId};
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

/// Attempts per request. rodbus channels reconnect in the background, so the
/// first request can race the initial TCP / TLS handshake.
const MAX_ATTEMPTS: u32 = 5;

/// Per-request response timeout.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Plain Modbus/TCP channel to `host:port`.
pub fn tcp_channel(host: &str, port: u16) -> Channel {
    spawn_tcp_client_task(
        HostAddr::dns(host.to_string(), port),
        1,
        rodbus::default_retry_strategy(),
        rodbus::DecodeLevel::default(),
        None,
    )
}

/// Modbus Security channel. Subject name + creds drive
/// `TlsClientConfig::full_pki`: the device's cert must chain to the
/// configured CA AND present a SAN/CN matching `subject_name`.
pub fn tls_channel(
    host: &str,
    port: u16,
    subject_name: &str,
    creds: &GatewayCredentials,
) -> Result<Channel> {
    let tls_config = tls::build_tls_config(
        subject_name,
        &creds.ca_bundle_path,
        &creds.cert_path,
        &creds.key_path,
    )?;
    Ok(spawn_tls_client_task(
        HostAddr::dns(host.to_string(), port),
        1,
        rodbus::default_retry_strategy(),
        tls_config,
        rodbus::DecodeLevel::default(),
        None,
    ))
}

/// Read `count` registers at `addr` with FC3 (holding) or FC4 (input).
pub async fn read_registers(
    channel: &mut Channel,
    unit_id: u8,
    addr: u16,
    count: u16,
    function: ReadFunction,
) -> Result<Vec<u16>> {
    channel.enable().await.context("modbus channel enable")?;
    let range = AddressRange::try_from(addr, count)
        .map_err(|e| anyhow::anyhow!("invalid modbus address range: {e}"))?;
    let param = RequestParam::new(UnitId::new(unit_id), RESPONSE_TIMEOUT);

    let mut last_err = None;
    for attempt in 0..MAX_ATTEMPTS {
        let result = match function {
            ReadFunction::Holding => channel.read_holding_registers(param, range).await,
            ReadFunction::Input => channel.read_input_registers(param, range).await,
        };
        match result {
            Ok(registers) => return Ok(registers.iter().map(|r| r.value).collect()),
            // Reason: an exception is the device's definite answer (e.g. an
            // address it doesn't have); asking again only stalls the
            // device's other readings behind the backoff.
            Err(e @ RequestError::Exception(_)) => {
                return Err(e).context(format!("modbus {function:?} read refused"));
            }
            Err(e) => {
                warn!(attempt, ?function, error = %e, "modbus read failed; retrying");
                last_err = Some(e);
                sleep(Duration::from_millis(500 * (1 << attempt))).await;
            }
        }
    }
    Err(last_err.unwrap()).context(format!("modbus {function:?} read exhausted retries"))
}

/// Write `words` at `addr` with FC6 (single register) or FC16 (multiple).
/// FC6 takes exactly one word; spec parse rejects wider types before here.
pub async fn write_registers(
    mut channel: Channel,
    unit_id: u8,
    addr: u16,
    words: &[u16],
    function: WriteFunction,
) -> Result<()> {
    channel.enable().await.context("modbus channel enable")?;
    let param = RequestParam::new(UnitId::new(unit_id), RESPONSE_TIMEOUT);
    let multiple = WriteMultiple::from(addr, words.to_vec())
        .map_err(|e| anyhow::anyhow!("invalid modbus write request: {e}"))?;

    let mut last_err = None;
    for attempt in 0..MAX_ATTEMPTS {
        let result = match function {
            WriteFunction::Single => channel
                .write_single_register(param, Indexed::new(addr, words[0]))
                .await
                .map(drop),
            WriteFunction::Multiple => channel
                .write_multiple_registers(param, multiple.clone())
                .await
                .map(drop),
        };
        match result {
            Ok(()) => return Ok(()),
            // Reason: an exception is the device's definite answer (e.g. an
            // address it doesn't have); asking again only stalls the
            // device's other readings behind the backoff.
            Err(e @ RequestError::Exception(_)) => {
                return Err(e).context(format!("modbus {function:?} write refused"));
            }
            Err(e) => {
                warn!(attempt, ?function, error = %e, "modbus write failed; retrying");
                last_err = Some(e);
                sleep(Duration::from_millis(500 * (1 << attempt))).await;
            }
        }
    }
    Err(last_err.unwrap()).context(format!("modbus {function:?} write exhausted retries"))
}
