//! In-process Redfish HTTPS+mTLS service for the e2e test.
//!
//! Axum router serving `/redfish/v1/Chassis/SW1/Thermal` with one
//! ReadingCelsius. axum-server + rustls `ServerConfig` configured with
//! `WebPkiClientVerifier` — requires CA-validated client cert per DSP0266
//! §13.3.5. Standards-compliant inbound mTLS.

use crate::fixtures::pki::TestPki;
use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::routing::get;
use axum_server::tls_rustls::RustlsConfig;
use rustls::pki_types::CertificateDer;
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use serde_json::{Value, json};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// Static temperature served at the Thermal endpoint.
pub const TEMPERATURE: f64 = 42.5;
/// Resource path the gateway dials (matches the redfish_tls_binding helper).
pub const RESOURCE_PATH: &str = "/Chassis/SW1/Thermal";
/// JSON Pointer the binding uses to drill into the response body.
pub const JSON_POINTER: &str = "/Temperatures/0/ReadingCelsius";

/// Returned by [`spawn`]: the loopback addr the gateway dials + the live
/// axum-server task handle (abort = shutdown).
pub struct RedfishSecurityFixture {
    /// Loopback addr the HTTPS service is listening on.
    pub addr: SocketAddr,
    /// Background task running the axum-server; aborted by drop.
    pub _handle: JoinHandle<()>,
}

impl Drop for RedfishSecurityFixture {
    fn drop(&mut self) {
        self._handle.abort();
    }
}

/// Spawn the in-process Redfish HTTPS+mTLS service. OS-assigned loopback
/// port; rustls config requires CA-validated client cert (mTLS, §13.3.5).
pub async fn spawn(pki: &TestPki) -> Result<RedfishSecurityFixture> {
    // Reason: bind-and-drop to pick an open port; axum_server takes a SocketAddr.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let addr = listener.local_addr()?;
    drop(listener);

    let server_config = build_server_config(pki)?;
    let rustls_config = RustlsConfig::from_config(Arc::new(server_config));

    let app = Router::new()
        .route("/redfish/v1/Chassis/SW1/Thermal", get(thermal_handler))
        .with_state(TEMPERATURE);

    let handle = tokio::spawn(async move {
        // Drives the HTTPS+mTLS server until the task is aborted.
        let _ = axum_server::bind_rustls(addr, rustls_config)
            .serve(app.into_make_service())
            .await;
    });
    // Reason: give the bind a tick to take so the test's first GET doesn't
    // race the listen() syscall.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    Ok(RedfishSecurityFixture {
        addr,
        _handle: handle,
    })
}

/// One-shot handler returning a Thermal resource with the canned reading.
async fn thermal_handler(State(temp): State<f64>) -> axum::Json<Value> {
    axum::Json(json!({
        "@odata.id": "/redfish/v1/Chassis/SW1/Thermal",
        "@odata.type": "#Thermal.v1_7_0.Thermal",
        "Id": "Thermal",
        "Name": "Thermal",
        "Temperatures": [{ "Name": "Inlet", "ReadingCelsius": temp }],
    }))
}

/// Build a rustls ServerConfig that requires a CA-validated client cert.
fn build_server_config(pki: &TestPki) -> Result<ServerConfig> {
    let ca_pem = std::fs::read(pki.ca_bundle.path()).context("read CA bundle")?;
    let mut ca_roots = RootCertStore::empty();
    for der in rustls_pemfile::certs(&mut ca_pem.as_slice()) {
        let der = der.context("parse CA cert")?;
        ca_roots.add(der).context("add CA to RootCertStore")?;
    }
    let client_verifier = WebPkiClientVerifier::builder(Arc::new(ca_roots))
        .build()
        .context("build WebPkiClientVerifier")?;

    let server_cert_pem = std::fs::read(pki.server_cert.path()).context("read server cert")?;
    let server_certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut server_cert_pem.as_slice())
            .collect::<std::result::Result<_, _>>()
            .context("parse server cert chain")?;

    let server_key_pem = std::fs::read(pki.server_key.path()).context("read server key")?;
    let server_key = rustls_pemfile::private_key(&mut server_key_pem.as_slice())
        .context("parse server private key")?
        .context("no private key in server key PEM")?;

    let config = ServerConfig::builder()
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(server_certs, server_key)
        .context("ServerConfig::with_single_cert")?;
    Ok(config)
}
