//! In-process DNP3/TLS outstation server for the e2e test.
//!
//! `dnp3::tcp::Server::new_tls_server` with `TlsServerConfig::full_pki` —
//! CA-validated mTLS per IEEE 1815 Annex E. Seeds one analog input point at
//! index 0 with a static value the gateway reads via Group30Var1.

use crate::fixtures::pki::TestPki;
use anyhow::Result;
use dnp3::app::control::{
    CommandStatus, Group12Var1, Group41Var1, Group41Var2, Group41Var3, Group41Var4,
};
use dnp3::app::measurement::{AnalogInput, Flags, Time};
use dnp3::app::{Listener, MaybeAsync};
use dnp3::link::{EndpointAddress, LinkErrorMode};
use dnp3::outstation::database::{
    Add, AnalogInputConfig, DatabaseHandle, EventAnalogInputVariation, EventBufferConfig,
    EventClass, StaticAnalogInputVariation, Update, UpdateOptions,
};
use dnp3::outstation::{
    ConnectionState, ControlHandler, ControlSupport, OperateType, OutstationApplication,
    OutstationConfig, OutstationInformation,
};
use dnp3::tcp::tls::{MinTlsVersion, TlsServerConfig};
use dnp3::tcp::{AddressFilter, Server, ServerHandle};
use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::TcpListener;

/// Static value seeded into the outstation's analog input at point 0. The
/// gateway reads this back over DNP3/TLS.
pub const POINT_VALUE: f64 = 12_345.0;

/// Result of [`spawn`]: the loopback addr the master dials + the live server
/// handle (drop = shutdown).
pub struct Dnp3SecurityFixture {
    /// Loopback addr the outstation TLS server is listening on.
    pub addr: SocketAddr,
    /// dnp3 server handle; drop terminates.
    pub _server: ServerHandle,
}

/// Spawn the in-process DNP3/TLS outstation. OS-assigned loopback port.
/// PKI material drives `TlsServerConfig::full_pki(client_subject_name=None,
/// ...)` — accepts any CA-validated master cert (no SAN check, since DNP3
/// doesn't carry a role extension like Modbus Security does).
pub async fn spawn(pki: &TestPki) -> Result<Dnp3SecurityFixture> {
    // Reason: bind-and-drop to learn an available port; `Server::new_tls_server`
    // takes a concrete SocketAddr.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let addr = listener.local_addr()?;
    drop(listener);

    let tls_config = TlsServerConfig::full_pki(
        None,
        pki.ca_bundle.path(),
        pki.server_cert.path(),
        pki.server_key.path(),
        None,
        MinTlsVersion::V13,
    )?;
    let mut server = Server::new_tls_server(LinkErrorMode::Close, addr, tls_config);
    let outstation = server.add_outstation(
        outstation_config(),
        Box::new(App),
        Box::new(Info),
        Box::new(Ctl),
        Box::new(NopListener),
        AddressFilter::Any,
    )?;
    // Seed the point + value the master will read back.
    outstation.transaction(|db| {
        db.add(
            0,
            Some(EventClass::Class1),
            AnalogInputConfig {
                s_var: StaticAnalogInputVariation::Group30Var1,
                e_var: EventAnalogInputVariation::Group32Var1,
                deadband: 0.0,
            },
        );
        db.update(
            0,
            &AnalogInput::new(POINT_VALUE, Flags::ONLINE, Time::synchronized(0)),
            UpdateOptions::default(),
        );
    });
    let handle = server.bind().await?;
    Ok(Dnp3SecurityFixture {
        addr,
        _server: handle,
    })
}

/// Outstation config — single master at addr 1, this outstation at 1024.
/// Mirrors `mock-dnp3-outstation` so the gateway client's master addr stays
/// consistent across plain + TLS e2e tests.
fn outstation_config() -> OutstationConfig {
    OutstationConfig::new(
        EndpointAddress::try_new(1024).expect("outstation addr"),
        EndpointAddress::try_new(1).expect("master addr"),
        EventBufferConfig::new(0, 0, 0, 0, 0, 5, 0, 0),
    )
}

/// Minimal OutstationApplication — defaults fine for read-only use.
struct App;
impl OutstationApplication for App {}

/// No-op OutstationInformation.
struct Info;
impl OutstationInformation for Info {}

/// No-op ControlHandler. Tier 1 is read-only; every select/operate returns
/// NotSupported.
struct Ctl;
impl ControlHandler for Ctl {}

macro_rules! reject_control {
    ($ty:ty) => {
        impl ControlSupport<$ty> for Ctl {
            fn select(
                &mut self,
                _control: $ty,
                _index: u16,
                _db: &mut DatabaseHandle,
            ) -> CommandStatus {
                CommandStatus::NotSupported
            }
            fn operate(
                &mut self,
                _control: $ty,
                _index: u16,
                _op_type: OperateType,
                _db: &mut DatabaseHandle,
            ) -> CommandStatus {
                CommandStatus::NotSupported
            }
        }
    };
}

reject_control!(Group12Var1);
reject_control!(Group41Var1);
reject_control!(Group41Var2);
reject_control!(Group41Var3);
reject_control!(Group41Var4);

/// No-op connection-state listener.
struct NopListener;
impl Listener<ConnectionState> for NopListener {
    fn update(&mut self, _state: ConnectionState) -> MaybeAsync<()> {
        MaybeAsync::ready(())
    }
}
