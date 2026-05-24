//! SNMP client wrapping `snmp2`. Plain v2c (community string, no auth) +
//! SNMPv3 USM (RFC 3414 / 7860 — HMAC-SHA-2 auth + AES-CFB priv).
//!
//! Branches on `(trust)` — `Snmpv3Usm` lights up the v3 USM path, else v2c.
//! USM passphrases are SECRETS — loaded from env vars keyed by
//! `security_name`, never from the DTM.

use crate::asyncapi::trust::{DeviceTrust, Snmpv3AuthProtocol, Snmpv3PrivProtocol};
use crate::asyncapi::types::SnmpBinding;
use crate::config::GatewayCredentials;
use anyhow::{Context, Result};
use snmp2::v3::{Auth, AuthProtocol, Cipher, Security};
use snmp2::{AsyncSession, Oid, Value};
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

/// Default community for SNMP v2c reads. Industrial gear typically allows
/// "public" for read-only access.
const COMMUNITY: &[u8] = b"public";
/// Retry policy mirrors the other protocols — first request can race UDP
/// arrival/processing.
const MAX_READ_ATTEMPTS: u32 = 5;

/// Full read pipeline for an SNMP measurement: resolve → GET → cast to f64.
///
/// `Some(Snmpv3Usm{..})` selects v3 USM; passphrases come from env vars
/// `SNMP_USM_<UPPERCASE_SECURITY_NAME>_AUTH_PASSPHRASE` and `_PRIV_PASSPHRASE`.
/// Anything else falls back to plain v2c (community = "public").
/// `creds` is unused for SNMP — USM is HMAC+symmetric, not PKI.
pub async fn read_measurement(
    b: &SnmpBinding,
    trust: Option<&DeviceTrust>,
    _creds: Option<&GatewayCredentials>,
) -> Result<f64> {
    let oid = parse_dotted_oid(&b.oid)?;
    let endpoint = format!("{}:{}", b.host, b.port);

    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..MAX_READ_ATTEMPTS {
        let outcome = match trust {
            Some(DeviceTrust::Snmpv3Usm {
                security_name,
                auth_protocol,
                priv_protocol,
            }) => {
                try_get_v3(
                    &endpoint,
                    &oid,
                    security_name,
                    *auth_protocol,
                    *priv_protocol,
                )
                .await
            }
            _ => try_get_v2c(&endpoint, &oid).await,
        };
        match outcome {
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

/// Single v2c GET attempt. Community string only — no auth, no encryption.
async fn try_get_v2c(endpoint: &str, oid: &Oid<'_>) -> Result<f64> {
    let mut sess = AsyncSession::new_v2c(endpoint, COMMUNITY, 0)
        .await
        .context("build snmp2 v2c session")?;
    let pdu = sess.get(oid).await.context("snmp v2c get")?;
    extract_integer(&pdu)
}

/// Single SNMPv3 USM GET attempt. Loads auth+priv passphrases from env vars
/// keyed by `security_name`; runs `init()` for engine-id discovery before
/// the actual GET.
async fn try_get_v3(
    endpoint: &str,
    oid: &Oid<'_>,
    security_name: &str,
    auth_protocol: Snmpv3AuthProtocol,
    priv_protocol: Snmpv3PrivProtocol,
) -> Result<f64> {
    let (auth_pass, priv_pass) = load_usm_passphrases(security_name)?;
    let security = Security::new(security_name.as_bytes(), auth_pass.as_bytes())
        .with_auth_protocol(map_auth_protocol(auth_protocol))
        .with_auth(Auth::AuthPriv {
            cipher: map_priv_protocol(priv_protocol),
            privacy_password: priv_pass.into_bytes(),
        });
    let mut sess = AsyncSession::new_v3(endpoint, 0, security)
        .await
        .context("build snmp2 v3 session")?;
    // engine-id discovery — sends an unauthenticated probe to learn the
    // authoritative engine id + boot/time counters.
    sess.init()
        .await
        .context("snmp v3 init / engine-id discovery")?;
    let pdu = sess.get(oid).await.context("snmp v3 get")?;
    extract_integer(&pdu)
}

/// Pull the first varbind's integer-shaped value out of a response PDU.
fn extract_integer(pdu: &snmp2::Pdu) -> Result<f64> {
    let (_oid, value) = pdu
        .varbinds
        .clone()
        .next()
        .context("snmp response had no varbinds")?;
    match value {
        Value::Integer(i) => Ok(i as f64),
        Value::Counter32(c) => Ok(f64::from(c)),
        Value::Unsigned32(u) => Ok(f64::from(u)),
        Value::Counter64(c) => Ok(c as f64),
        other => anyhow::bail!("expected integer-shaped SNMP value, got {other:?}"),
    }
}

/// Parse a dotted-numeric OID string (e.g. "1.3.6.1.4.1.41999.1.1.0") into
/// snmp2's borrowed `Oid<'static>`.
fn parse_dotted_oid(s: &str) -> Result<Oid<'static>> {
    let parts: Vec<u64> = s
        .split('.')
        .map(|segment| segment.parse::<u64>())
        .collect::<std::result::Result<_, _>>()
        .with_context(|| format!("parse OID dotted-numeric: {s}"))?;
    Oid::from(&parts).map_err(|e| anyhow::anyhow!("invalid OID {s}: {e:?}"))
}

/// Map our `DeviceTrust::Snmpv3AuthProtocol` to snmp2's `AuthProtocol`.
fn map_auth_protocol(a: Snmpv3AuthProtocol) -> AuthProtocol {
    match a {
        Snmpv3AuthProtocol::Sha256 => AuthProtocol::Sha256,
        Snmpv3AuthProtocol::Sha384 => AuthProtocol::Sha384,
        Snmpv3AuthProtocol::Sha512 => AuthProtocol::Sha512,
    }
}

/// Map our `DeviceTrust::Snmpv3PrivProtocol` to snmp2's `Cipher`.
fn map_priv_protocol(p: Snmpv3PrivProtocol) -> Cipher {
    match p {
        Snmpv3PrivProtocol::Aes128 => Cipher::Aes128,
        Snmpv3PrivProtocol::Aes192 => Cipher::Aes192,
        Snmpv3PrivProtocol::Aes256 => Cipher::Aes256,
    }
}

/// Load USM auth + priv passphrases from env vars keyed by `security_name`.
/// Env names are `SNMP_USM_<UPPERCASE_SECURITY_NAME>_AUTH_PASSPHRASE` and
/// `_PRIV_PASSPHRASE`. Both required for `authPriv`.
fn load_usm_passphrases(security_name: &str) -> Result<(String, String)> {
    let key = security_name.to_uppercase().replace('-', "_");
    let auth_var = format!("SNMP_USM_{key}_AUTH_PASSPHRASE");
    let priv_var = format!("SNMP_USM_{key}_PRIV_PASSPHRASE");
    let auth_pass =
        std::env::var(&auth_var).with_context(|| format!("missing required env: {auth_var}"))?;
    let priv_pass =
        std::env::var(&priv_var).with_context(|| format!("missing required env: {priv_var}"))?;
    Ok((auth_pass, priv_pass))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dotted_oid_handles_sysuptime() {
        // Arrange + Act
        let oid = parse_dotted_oid("1.3.6.1.2.1.1.3.0").unwrap();
        // Assert — round-trip back to dotted form
        let s = oid.to_string();
        assert_eq!(s, "1.3.6.1.2.1.1.3.0");
    }

    #[test]
    fn load_usm_passphrases_pulls_from_env_keyed_by_security_name() {
        // Arrange
        unsafe {
            std::env::set_var("SNMP_USM_GW_TEST_AUTH_PASSPHRASE", "authsecret");
            std::env::set_var("SNMP_USM_GW_TEST_PRIV_PASSPHRASE", "privsecret");
        }
        // Act
        let (auth, priv_) = load_usm_passphrases("gw-test").unwrap();
        // Assert — uppercase + hyphens-to-underscores normalization works
        assert_eq!(auth, "authsecret");
        assert_eq!(priv_, "privsecret");
    }
}
