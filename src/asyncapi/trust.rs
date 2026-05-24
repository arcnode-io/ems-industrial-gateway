//! Per-device trust block (`x-device-trust`) — south-side mutual-auth contract.
//!
//! Generic across all protocols: one trust entry per device_id, parallel to
//! `x-protocol-source`. Variant discriminated by `trust_mode`. MVP-scoped:
//! `tls_mutual` only (subject name validated against CA-signed cert).
//! `snmpv3_usm` + `none` slot in once their contract items resolve.

use serde::Deserialize;

/// Per-device trust material. Variant discriminated by `trust_mode`.
///
/// Standards-compliant: TLS protocols use X.509 PKI (`tls_mutual` variant).
/// SNMPv3 uses USM (`snmpv3_usm`) per RFC 3414/7860 — HMAC + symmetric crypto,
/// not PKI. Both variants declare identity in the DTM; secrets (TLS private
/// key, USM passphrases) live in env / mounted-secrets per CLAUDE.md.
///
/// Revocation = redeploy DTM (per PM contract). `Clone` is needed because
/// the trust block moves into each per-measurement task, same as
/// `ProtocolBinding`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "trust_mode", rename_all = "snake_case")]
pub enum DeviceTrust {
    /// Mutual TLS via CA-signed certs (Modbus/DNP3/Redfish). Gateway validates
    /// the device cert chain against the CA bundle in cfg, and checks that the
    /// presented cert's SAN/CN matches `subject_name`.
    TlsMutual {
        /// Subject name (SAN or CN) the device's cert must present. Standard
        /// PKI identity check — same shape every PKI-capable device uses.
        subject_name: String,
    },
    /// SNMPv3 USM (RFC 3414 + RFC 7860) — HMAC-based auth + symmetric priv.
    /// authPriv only; deprecated MD5/DES are not exposed in the enums.
    /// Passphrases ARE secrets — looked up from env vars keyed by
    /// `security_name`, never in the DTM.
    Snmpv3Usm {
        /// USM user name. Matches the user the device's SNMPv3 agent has
        /// configured. Also keys the env-var lookup for auth + priv pass.
        security_name: String,
        /// HMAC-SHA-2 family. Avoid MD5/SHA1 (deprecated for new deployments).
        auth_protocol: Snmpv3AuthProtocol,
        /// AES-CFB family. Avoid DES (deprecated).
        priv_protocol: Snmpv3PrivProtocol,
    },
}

/// SNMPv3 USM authentication protocol (RFC 3414 / RFC 7860).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Snmpv3AuthProtocol {
    /// HMAC-SHA-256-192 (RFC 7860).
    Sha256,
    /// HMAC-SHA-384-256 (RFC 7860).
    Sha384,
    /// HMAC-SHA-512-384 (RFC 7860).
    Sha512,
}

/// SNMPv3 USM privacy (encryption) protocol.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Snmpv3PrivProtocol {
    /// AES-128 CFB (RFC 3826).
    Aes128,
    /// AES-192 CFB (RFC 8264 / common extension).
    Aes192,
    /// AES-256 CFB (RFC 8264 / common extension).
    Aes256,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_tls_mutual_trust_from_json() {
        // Arrange — `x-device-trust[device_id]` block from device-api.
        let json = r#"{
            "trust_mode": "tls_mutual",
            "subject_name": "meter-01.acme-site.local"
        }"#;
        // Act
        let trust: DeviceTrust = serde_json::from_str(json).unwrap();
        // Assert — TlsMutual carries the SAN/CN to validate the cert against
        let DeviceTrust::TlsMutual { subject_name } = trust else {
            panic!("expected TlsMutual");
        };
        assert_eq!(subject_name, "meter-01.acme-site.local");
    }

    #[test]
    fn deserialize_snmpv3_usm_trust_from_json() {
        // Arrange — SNMPv3 USM trust block. Passphrases are NOT in DTM —
        // they're env-var secrets keyed by security_name.
        let json = r#"{
            "trust_mode": "snmpv3_usm",
            "security_name": "gateway",
            "auth_protocol": "sha256",
            "priv_protocol": "aes128"
        }"#;
        // Act
        let trust: DeviceTrust = serde_json::from_str(json).unwrap();
        // Assert — all three USM fields land
        let DeviceTrust::Snmpv3Usm {
            security_name,
            auth_protocol,
            priv_protocol,
        } = trust
        else {
            panic!("expected Snmpv3Usm");
        };
        assert_eq!(security_name, "gateway");
        assert!(matches!(auth_protocol, Snmpv3AuthProtocol::Sha256));
        assert!(matches!(priv_protocol, Snmpv3PrivProtocol::Aes128));
    }
}
