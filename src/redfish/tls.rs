//! Redfish HTTPS+mTLS client builder.
//!
//! DSP0266 §13.1 requires TLS; §13.3.5 lists client-cert auth as a supported
//! inbound authentication option. We use mTLS so machine-to-machine talks
//! without per-device passwords. CA chain + identity loaded from cfg paths.

use crate::config::GatewayCredentials;
use anyhow::{Context, Result};
use reqwest::{Certificate, Client, Identity};
use std::fs;
use std::time::Duration;

/// Build a reqwest Client configured for Redfish HTTPS+mTLS.
///
/// CA bundle = trust anchor for the Redfish service's server cert.
/// Identity (cert + key concatenated into one PEM buffer) = the gateway's
/// client cert presented to the service per DSP0266 §13.3.5. Backend is
/// rustls (per `reqwest`'s `rustls-tls` feature on the gateway).
pub fn build_https_client(creds: &GatewayCredentials) -> Result<Client> {
    let ca_pem = fs::read(&creds.ca_bundle_path)
        .with_context(|| format!("read CA bundle at {:?}", creds.ca_bundle_path))?;
    let ca = Certificate::from_pem(&ca_pem).context("parse CA bundle PEM")?;

    let cert_pem = fs::read(&creds.cert_path)
        .with_context(|| format!("read gateway cert at {:?}", creds.cert_path))?;
    let key_pem = fs::read(&creds.key_path)
        .with_context(|| format!("read gateway key at {:?}", creds.key_path))?;
    // reqwest Identity::from_pem wants key + cert in ONE PEM buffer.
    let mut combined = key_pem;
    combined.extend_from_slice(&cert_pem);
    let identity = Identity::from_pem(&combined)
        .context("build reqwest Identity from gateway cert/key PEM")?;

    Client::builder()
        .timeout(Duration::from_secs(5))
        .use_rustls_tls()
        .tls_built_in_root_certs(false)
        .add_root_certificate(ca)
        .identity(identity)
        .build()
        .context("build reqwest HTTPS+mTLS Client")
}
