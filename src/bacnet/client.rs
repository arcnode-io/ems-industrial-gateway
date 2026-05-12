//! BACnet/IP master client. Tier 1: one-shot `ReadProperty` of
//! `present_value` on an `AnalogInput` object, returned as `f64`.

use crate::asyncapi::types::BacnetIpBinding;
use anyhow::{Context, Result};
use bacnet_rs::app::Apdu;
use bacnet_rs::network::Npdu;
use bacnet_rs::object::{ObjectIdentifier, ObjectType, PropertyIdentifier};
use bacnet_rs::property::PropertyValue;
use bacnet_rs::service::{ConfirmedServiceChoice, ReadPropertyRequest, ReadPropertyResponse};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::{UdpSocket, lookup_host};
use tokio::time::{sleep, timeout};
use tracing::warn;

/// Same retry curve as the other protocols.
const MAX_READ_ATTEMPTS: u32 = 5;
/// How long to wait for a `ComplexAck` before retrying.
const READ_TIMEOUT: Duration = Duration::from_millis(800);
/// Invoke id used on every request. We send one at a time so collision is
/// not a concern.
const INVOKE_ID: u8 = 1;

/// Full read pipeline for a BACnet measurement.
pub async fn read_measurement(b: &BacnetIpBinding) -> Result<f64> {
    if b.object_type != "analog_input" {
        anyhow::bail!(
            "Tier 1 BACnet only supports analog_input object_type, got {}",
            b.object_type
        );
    }
    if b.property_id != "present_value" {
        anyhow::bail!(
            "Tier 1 BACnet only supports present_value property_id, got {}",
            b.property_id
        );
    }

    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..MAX_READ_ATTEMPTS {
        match try_read(b).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                warn!(attempt, error = %e, "bacnet read failed; retrying");
                last_err = Some(e);
                sleep(Duration::from_millis(500 * (1u64 << attempt))).await;
            }
        }
    }
    Err(last_err.unwrap()).context("bacnet read exhausted retries")
}

/// Single attempt — bind ephemeral UDP matching the target's address family,
/// send `ReadProperty`, wait for `ComplexAck`.
async fn try_read(b: &BacnetIpBinding) -> Result<f64> {
    let target: SocketAddr = lookup_host((b.host.as_str(), b.port))
        .await
        .context("resolve bacnet host:port")?
        .next()
        .context("no addrs from bacnet host:port resolution")?;
    let bind_addr = if target.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind_addr).await?;
    let object_identifier = ObjectIdentifier::new(ObjectType::AnalogInput, b.object_instance);
    let request = ReadPropertyRequest::new(object_identifier, PropertyIdentifier::PresentValue);
    let mut svc = Vec::new();
    request
        .encode(&mut svc)
        .map_err(|e| anyhow::anyhow!("rp encode: {e:?}"))?;
    let apdu = Apdu::ConfirmedRequest {
        segmented: false,
        more_follows: false,
        segmented_response_accepted: true,
        max_segments: bacnet_rs::app::MaxSegments::Unspecified,
        max_response_size: bacnet_rs::app::MaxApduSize::Up1476,
        invoke_id: INVOKE_ID,
        sequence_number: None,
        proposed_window_size: None,
        service_choice: ConfirmedServiceChoice::ReadProperty,
        service_data: svc,
    };
    let frame = wrap_bvlc(apdu.encode());
    socket.send_to(&frame, target).await?;

    let mut buf = [0u8; 1500];
    let (n, _) = timeout(READ_TIMEOUT, socket.recv_from(&mut buf))
        .await
        .context("bacnet recv timeout")??;
    parse_response(&buf[..n])
}

/// Decode an inbound BACnet/IP frame; extract the `Real` present_value.
fn parse_response(frame: &[u8]) -> Result<f64> {
    if frame.len() < 4 || frame[0] != 0x81 {
        anyhow::bail!("not a BACnet/IP BVLC frame");
    }
    let (_npdu, npdu_len) =
        Npdu::decode(&frame[4..]).map_err(|e| anyhow::anyhow!("npdu decode: {e:?}"))?;
    let apdu_bytes = &frame[4 + npdu_len..];
    let apdu = Apdu::decode(apdu_bytes).map_err(|e| anyhow::anyhow!("apdu decode: {e:?}"))?;
    let service_data = match apdu {
        Apdu::ComplexAck {
            service_choice: ConfirmedServiceChoice::ReadProperty,
            service_data,
            ..
        } => service_data,
        Apdu::Error {
            error_class,
            error_code,
            ..
        } => {
            anyhow::bail!("bacnet error: class={error_class} code={error_code}")
        }
        _ => anyhow::bail!("unexpected apdu type"),
    };
    let resp = ReadPropertyResponse::decode(&service_data)
        .map_err(|e| anyhow::anyhow!("rp resp decode: {e:?}"))?;
    let value = resp
        .property_values
        .into_iter()
        .find_map(|v| match v {
            PropertyValue::Real(f) => Some(f as f64),
            PropertyValue::Double(f) => Some(f),
            PropertyValue::Unsigned(u) => Some(u as f64),
            PropertyValue::Signed(i) => Some(i as f64),
            _ => None,
        })
        .context("no numeric value in response")?;
    Ok(value)
}

/// Wrap an APDU in NPDU + BVLC (`0x81 0x0A`, Original-Unicast-NPDU).
fn wrap_bvlc(apdu: Vec<u8>) -> Vec<u8> {
    let mut npdu = Npdu::new();
    npdu.control.expecting_reply = true;
    let npdu_bytes = npdu.encode();
    let mut payload = npdu_bytes;
    payload.extend_from_slice(&apdu);
    let mut out = vec![0x81, 0x0A, 0x00, 0x00];
    out.extend_from_slice(&payload);
    let total = out.len() as u16;
    out[2] = (total >> 8) as u8;
    out[3] = (total & 0xFF) as u8;
    out
}
