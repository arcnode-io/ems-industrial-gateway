//! Modbus/TLS client config builder — standards-compliant X.509 PKI.
//!
//! Per Modbus Security spec (IEC 62443 / "Modbus Messaging on TCP/IP — Security
//! Considerations"), Modbus/TLS uses mutual TLS with CA-signed X.509 certs.
//! Gateway loads its own cert + key + the CA bundle; verifies the device's
//! presented cert chains to the CA and matches the expected subject name
//! (SAN/CN) from `x-device-trust[device_id].subject_name`.

use anyhow::{Context, Result};
use rodbus::client::{MinTlsVersion, TlsClientConfig};
use std::path::Path;

/// Build a CA-validated TLS client config for a Modbus/TLS dial.
///
/// `server_subject_name` is the SAN/CN the device's cert must present
/// (per-device, from the DTM trust block).
/// `ca_bundle_path` is the global CA trust anchor (from cfg, mounted secret).
/// `gateway_cert_path` / `gateway_key_path` are the gateway's own
/// CA-issued cert + matching key (cfg paths; content is secret).
/// TLS 1.3 floor, per PM-approved contract.
pub fn build_tls_config(
    server_subject_name: &str,
    ca_bundle_path: &Path,
    gateway_cert_path: &Path,
    gateway_key_path: &Path,
) -> Result<TlsClientConfig> {
    TlsClientConfig::full_pki(
        Some(server_subject_name.to_string()),
        ca_bundle_path,
        gateway_cert_path,
        gateway_key_path,
        None,
        MinTlsVersion::V1_3,
    )
    .context("build rodbus TlsClientConfig::full_pki")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, KeyUsagePurpose};
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Write a string to a NamedTempFile and hand back the handle (keeps the
    /// file alive for the caller's scope).
    fn write_tempfile(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    /// Build a CA cert + key with KeyCertSign usage (IsCa::Ca).
    fn build_ca() -> (rcgen::Certificate, KeyPair) {
        let mut params = CertificateParams::new(vec![]).unwrap();
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "test-ca");
        params.distinguished_name = dn;
        let key_pair = KeyPair::generate().unwrap();
        let cert = params.self_signed(&key_pair).unwrap();
        (cert, key_pair)
    }

    /// Build a leaf cert signed by the given CA, with `cn` as the subject CN.
    fn build_leaf_signed_by(
        cn: &str,
        ca_cert: &rcgen::Certificate,
        ca_key: &KeyPair,
    ) -> (rcgen::Certificate, KeyPair) {
        let mut params = CertificateParams::new(vec![cn.to_string()]).unwrap();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, cn);
        params.distinguished_name = dn;
        let key_pair = KeyPair::generate().unwrap();
        let cert = params.signed_by(&key_pair, ca_cert, ca_key).unwrap();
        (cert, key_pair)
    }

    #[test]
    fn build_tls_config_accepts_ca_bundle_and_gateway_chain() {
        // rustls 0.23 wants a process-wide crypto provider installed before
        // any TLS work — idempotent in case another test already did it.
        let _ = rustls::crypto::ring::default_provider().install_default();
        // Arrange — stand up a tiny PKI: CA self-signs, CA issues gateway cert.
        let (ca_cert, ca_key) = build_ca();
        let (gateway_cert, gateway_key) = build_leaf_signed_by("gateway.test", &ca_cert, &ca_key);

        let ca_bundle_file = write_tempfile(&ca_cert.pem());
        let gateway_cert_file = write_tempfile(&gateway_cert.pem());
        let gateway_key_file = write_tempfile(&gateway_key.serialize_pem());

        // Act
        let result = build_tls_config(
            "meter-01.acme-site.local",
            ca_bundle_file.path(),
            gateway_cert_file.path(),
            gateway_key_file.path(),
        );

        // Assert — builder accepts valid CA bundle + gateway chain
        assert!(
            result.is_ok(),
            "build_tls_config failed: {:?}",
            result.err()
        );
    }
}
