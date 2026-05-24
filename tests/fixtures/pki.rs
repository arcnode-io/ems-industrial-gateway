//! PKI fixture: generates a tiny CA + leaf certs for Modbus Security e2e.
//!
//! Gateway leaf carries the Modbus Role extension (OID
//! `1.3.6.1.4.1.50316.802.1`, UTF8String "Operator") — required by
//! rodbus's `spawn_tls_server_task_with_authz` for the authz path.

use anyhow::Result;
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, CustomExtension, DistinguishedName, DnType,
    IsCa, KeyPair, KeyUsagePurpose, SanType,
};
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr};
use tempfile::NamedTempFile;

/// OID for the Modbus Role X.509 extension (per Modbus Security spec).
const MODBUS_ROLE_OID: &[u64] = &[1, 3, 6, 1, 4, 1, 50316, 802, 1];

/// Materialized PKI tempfiles for one e2e run. Files exist while the struct
/// is alive; drop deletes them.
pub struct TestPki {
    /// CA root cert (PEM). Shared trust anchor for both ends.
    pub ca_bundle: NamedTempFile,
    /// Server-side cert (Modbus server presents this).
    pub server_cert: NamedTempFile,
    /// Server-side private key (PKCS#8 PEM).
    pub server_key: NamedTempFile,
    /// Gateway-side cert (Modbus client presents this) — carries Role ext.
    pub gateway_cert: NamedTempFile,
    /// Gateway-side private key (PKCS#8 PEM).
    pub gateway_key: NamedTempFile,
}

/// Generate a fresh CA + two CA-signed leaf certs, write to tempfiles.
/// Server SAN = `server_subject_name` (caller chooses to match the
/// gateway's `x-device-trust[device].subject_name`).
/// Gateway CN = `gateway.test.local` plus a Modbus Role extension with
/// role "Operator" — the read-only role expected by Modbus Security's
/// `ReadOnlyAuthorizationHandler`. (Harmless for other protocols.)
pub fn gen_test_pki(server_subject_name: &str) -> Result<TestPki> {
    // rustls 0.23 requires a process-wide crypto provider be installed once
    // before any TLS handshake. Idempotent + safe to call from every test.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (ca_cert, ca_key) = build_ca()?;
    // Server gets the requested DNS SAN + 127.0.0.1 IP SAN. The IP SAN is
    // what reqwest (Redfish HTTPS path) verifies against — the URL is dialled
    // by loopback IP. rodbus + dnp3 paths verify against the DNS SAN via
    // `full_pki(Some(subject_name))`, so both are satisfied by the same cert.
    let (server_cert, server_key) = build_leaf(
        server_subject_name,
        vec![SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST))],
        None,
        &ca_cert,
        &ca_key,
    )?;
    let role_ext = modbus_role_extension("Operator");
    let (gateway_cert, gateway_key) = build_leaf(
        "gateway.test.local",
        vec![],
        Some(role_ext),
        &ca_cert,
        &ca_key,
    )?;

    Ok(TestPki {
        ca_bundle: write_tempfile(&ca_cert.pem())?,
        server_cert: write_tempfile(&server_cert.pem())?,
        server_key: write_tempfile(&server_key.serialize_pem())?,
        gateway_cert: write_tempfile(&gateway_cert.pem())?,
        gateway_key: write_tempfile(&gateway_key.serialize_pem())?,
    })
}

/// Self-signed CA cert + key with KeyCertSign usage.
fn build_ca() -> Result<(Certificate, KeyPair)> {
    let mut params = CertificateParams::new(vec![])?;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "ems-test-ca");
    params.distinguished_name = dn;
    let key_pair = KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;
    Ok((cert, key_pair))
}

/// CA-signed leaf cert. `cn` becomes the CN + a DNS SAN. `extra_sans` adds
/// IP/DNS SANs (e.g. 127.0.0.1 so reqwest's URL-based hostname check passes
/// when the test dials by loopback IP). Optionally carries a custom ext.
fn build_leaf(
    cn: &str,
    extra_sans: Vec<SanType>,
    extra_ext: Option<CustomExtension>,
    ca_cert: &Certificate,
    ca_key: &KeyPair,
) -> Result<(Certificate, KeyPair)> {
    let mut params = CertificateParams::new(vec![cn.to_string()])?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, cn);
    params.distinguished_name = dn;
    for san in extra_sans {
        params.subject_alt_names.push(san);
    }
    if let Some(ext) = extra_ext {
        params.custom_extensions.push(ext);
    }
    let key_pair = KeyPair::generate()?;
    let cert = params.signed_by(&key_pair, ca_cert, ca_key)?;
    Ok((cert, key_pair))
}

/// Encode the Modbus Role extension content (UTF8String per spec).
fn modbus_role_extension(role: &str) -> CustomExtension {
    let content = yasna::construct_der(|writer| {
        writer.write_utf8_string(role);
    });
    CustomExtension::from_oid_content(MODBUS_ROLE_OID, content)
}

/// Write content to a tempfile and return the handle.
fn write_tempfile(content: &str) -> Result<NamedTempFile> {
    let mut f = NamedTempFile::new()?;
    f.write_all(content.as_bytes())?;
    f.flush()?;
    Ok(f)
}
