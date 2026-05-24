//! DNP3/TLS client config builder — standards-compliant X.509 PKI per
//! IEEE 1815-2012 Annex E. Mirrors `modbus::tls` shape so the dispatcher
//! treats both protocols identically.

use crate::config::GatewayCredentials;
use anyhow::{Context, Result};
use dnp3::tcp::tls::{MinTlsVersion, TlsClientConfig};

/// Build a CA-validated TLS client config for a DNP3/TLS dial. The outstation's
/// cert must chain to `creds.ca_bundle_path` AND present a SAN/CN matching
/// `subject_name`. TLS 1.3 floor.
pub fn build_tls_config(subject_name: &str, creds: &GatewayCredentials) -> Result<TlsClientConfig> {
    TlsClientConfig::full_pki(
        Some(subject_name.to_string()),
        &creds.ca_bundle_path,
        &creds.cert_path,
        &creds.key_path,
        None,
        MinTlsVersion::V13,
    )
    .context("build dnp3 TlsClientConfig::full_pki")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asyncapi::trust::DeviceTrust;
    use rcgen::{
        BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair,
        KeyUsagePurpose,
    };
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn write_tempfile(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn build_tls_config_accepts_ca_validated_gateway_chain() {
        // rustls 0.23 wants a process-wide crypto provider installed before
        // any TLS work — idempotent in case another test already did it.
        let _ = rustls::crypto::ring::default_provider().install_default();
        // Arrange — gen a CA, sign a gateway cert with it.
        let mut ca_params = CertificateParams::new(vec![]).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let mut ca_dn = DistinguishedName::new();
        ca_dn.push(DnType::CommonName, "test-ca");
        ca_params.distinguished_name = ca_dn;
        let ca_key = KeyPair::generate().unwrap();
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();

        let mut gw_params = CertificateParams::new(vec!["gateway.test".to_string()]).unwrap();
        let mut gw_dn = DistinguishedName::new();
        gw_dn.push(DnType::CommonName, "gateway.test");
        gw_params.distinguished_name = gw_dn;
        let gw_key = KeyPair::generate().unwrap();
        let gw_cert = gw_params.signed_by(&gw_key, &ca_cert, &ca_key).unwrap();

        let ca_file = write_tempfile(&ca_cert.pem());
        let cert_file = write_tempfile(&gw_cert.pem());
        let key_file = write_tempfile(&gw_key.serialize_pem());
        let creds = GatewayCredentials {
            ca_bundle_path: PathBuf::from(ca_file.path()),
            cert_path: PathBuf::from(cert_file.path()),
            key_path: PathBuf::from(key_file.path()),
        };

        // Act
        let result = build_tls_config("outstation-01.test.local", &creds);

        // Assert — builder accepts valid CA bundle + gateway chain
        assert!(
            result.is_ok(),
            "build_tls_config failed: {:?}",
            result.err()
        );
        // Pacify the unused-import lint when DeviceTrust isn't otherwise touched.
        let _ = std::mem::size_of::<DeviceTrust>();
    }
}
