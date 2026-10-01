//! e2e: one Modbus session per device. High-risk: a session per read means a
//! meter polled four ways at 2 Hz sees eight TCP connects a second, and a
//! real meter or RTU accepts only a few sessions.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::asyncapi::types::ModbusTcpBinding;
use ems_industrial_gateway::modbus::client::read_measurement;
use rodbus::server::{
    AddressFilter, RequestHandler, ServerHandle, ServerHandlerMap, spawn_tcp_server_task,
};
use rodbus::{DecodeLevel, ExceptionCode, UnitId};
use serde_json::{Value, json};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};

const SITE_ID: &str = "site_001";

/// Holding registers 0-3 read 100-103; anything else is an illegal address.
struct Meter;
impl RequestHandler for Meter {
    fn read_holding_register(&self, address: u16) -> Result<u16, ExceptionCode> {
        (address < 4)
            .then_some(100 + address)
            .ok_or(ExceptionCode::IllegalDataAddress)
    }
}

async fn spawn_meter() -> Result<(SocketAddr, ServerHandle)> {
    // Reason: bind-and-drop to learn a free port; rodbus takes a concrete addr.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let addr = listener.local_addr()?;
    drop(listener);
    let map = ServerHandlerMap::single(UnitId::new(1), Meter.wrap());
    let server =
        spawn_tcp_server_task(16, addr, map, AddressFilter::Any, DecodeLevel::default()).await?;
    Ok((addr, server))
}

/// A TCP relay in front of `upstream` that counts connections it accepts.
async fn spawn_counting_relay(upstream: SocketAddr, accepted: Arc<AtomicUsize>) -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(async move {
        while let Ok((mut inbound, _)) = listener.accept().await {
            accepted.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                if let Ok(mut outbound) = TcpStream::connect(upstream).await {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
            });
        }
    });
    Ok(port)
}

fn reading(port: u16, address: u16) -> Value {
    json!({
        "unit": "none", "poll_rate_hz": 2.0, "protocol": "modbus_tcp",
        "host": "127.0.0.1", "port": port, "unit_id": "1", "function_code": 3,
        "address": address, "data_type": "uint16", "scale": 1.0, "offset": 0.0,
    })
}

#[tokio::test]
async fn a_device_reuses_one_modbus_session() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    // Arrange — four readings on one meter at 2 Hz, through a counting relay
    let (meter, _server) = spawn_meter().await?;
    let accepted = Arc::new(AtomicUsize::new(0));
    let port = spawn_counting_relay(meter, accepted.clone()).await?;
    let readings = (0..4)
        .map(|a| (format!("reg_{a}"), reading(port, a)))
        .collect();
    // Act — about 24 reads over 3 s
    fixtures::gateway::poll_for(SITE_ID, readings, 3).await?;
    // Assert
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "TCP sessions opened to one meter"
    );
    Ok(())
}

#[tokio::test]
async fn a_modbus_exception_fails_at_once() -> Result<()> {
    // Arrange — a binding at an address the meter doesn't have
    let (meter, _server) = spawn_meter().await?;
    let binding: ModbusTcpBinding = serde_json::from_value(json!({
        "host": "127.0.0.1", "port": meter.port(), "unit_id": "1", "function_code": 3,
        "address": 99, "data_type": "uint16", "scale": 1.0, "offset": 0.0,
    }))?;
    // Act
    let started = Instant::now();
    let result = read_measurement(&binding, None, None).await;
    // Assert — a definite answer from the device: no retries, no backoff
    assert!(result.is_err());
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "took {:?}",
        started.elapsed()
    );
    Ok(())
}
