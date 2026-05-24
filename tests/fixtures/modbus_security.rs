//! In-process Modbus Security (TLS + Role authz) server for the e2e test.
//!
//! Spins a `rodbus::server::spawn_tls_server_task_with_authz` with
//! `ReadOnlyAuthorizationHandler` against an inline `MeterHandler` whose
//! holding-register map returns int32 `1_000_000` at address 4000.
//! Bypasses the fixture container so the test is self-contained.

use crate::fixtures::pki::TestPki;
use anyhow::Result;
use rodbus::server::{
    AddressFilter, CertificateMode, MinTlsVersion, ReadOnlyAuthorizationHandler, RequestHandler,
    ServerHandle, ServerHandlerMap, TlsServerConfig, spawn_tls_server_task_with_authz,
};
use rodbus::{DecodeLevel, ExceptionCode, UnitId};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::TcpListener;

/// Modbus unit id the in-process server responds on.
pub const UNIT_ID: u8 = 1;
/// Holding-register map address that encodes int32 1_000_000 (high|low).
pub const REGISTER_ADDR: u16 = 4000;

/// Inline RequestHandler with a static holding map. Read-only.
struct StaticMeter {
    holding: HashMap<u16, u16>,
}
impl RequestHandler for StaticMeter {
    fn read_holding_register(&self, address: u16) -> Result<u16, ExceptionCode> {
        self.holding
            .get(&address)
            .copied()
            .ok_or(ExceptionCode::IllegalDataAddress)
    }
}

/// Returned by [`spawn`]: the bound address the gateway dials + the live
/// server handle (drop = shutdown).
pub struct ModbusSecurityFixture {
    /// Loopback address (`127.0.0.1:<port>`) the server is listening on.
    pub addr: SocketAddr,
    /// rodbus server task handle; drop terminates the server.
    pub _server: ServerHandle,
}

/// Spawn the in-process Modbus Security server. Listens on an
/// OS-assigned loopback port. PKI material drives `TlsServerConfig::new(...,
/// CertificateMode::AuthorityBased)` — CA-validated mTLS per Modbus Security.
pub async fn spawn(pki: &TestPki) -> Result<ModbusSecurityFixture> {
    // Reason: bind-and-drop to learn an available port; rodbus's spawn API
    // takes a concrete SocketAddr and doesn't surface the bound port itself.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let addr = listener.local_addr()?;
    drop(listener);

    let mut holding = HashMap::new();
    // int32 1_000_000 = 0x000F4240 → high=0x000F, low=0x4240.
    holding.insert(REGISTER_ADDR, 0x000F);
    holding.insert(REGISTER_ADDR + 1, 0x4240);
    let handler = StaticMeter { holding }.wrap();
    let map = ServerHandlerMap::single(UnitId::new(UNIT_ID), handler);

    let tls_config = TlsServerConfig::new(
        pki.ca_bundle.path(),
        pki.server_cert.path(),
        pki.server_key.path(),
        None,
        MinTlsVersion::V1_3,
        CertificateMode::AuthorityBased,
    )?;
    let server = spawn_tls_server_task_with_authz(
        16,
        addr,
        map,
        ReadOnlyAuthorizationHandler::create(),
        tls_config,
        AddressFilter::Any,
        DecodeLevel::default(),
    )
    .await?;
    Ok(ModbusSecurityFixture {
        addr,
        _server: server,
    })
}
