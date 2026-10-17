//! e2e: a template-declared heartbeat (`x-heartbeat-source`), e.g. the CATL
//! BMS's 0x0380 counter. The gateway writes 0, 1, … modulo-1, 0, … once a
//! period, and never to a locked-out device.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{start_hivemq, unique_network};
use fixtures::spec_stub::spawn_lockable_stub;
use rodbus::server::{
    AddressFilter, RequestHandler, ServerHandle, ServerHandlerMap, spawn_tcp_server_task,
};
use rodbus::{DecodeLevel, ExceptionCode, Indexed, UnitId};
use serde_json::{Value, json};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

const BMS: &str = "bess_catl_1";
/// CATL EMS heartbeat register, 0x0380.
const HEARTBEAT: u16 = 896;
/// CATL's ETH board answers as unit 0x64.
const UNIT: u8 = 100;

/// A BMS that records the values written to its heartbeat register.
struct Bms(Arc<Mutex<Vec<u16>>>);

impl RequestHandler for Bms {
    fn write_single_register(&mut self, value: Indexed<u16>) -> Result<(), ExceptionCode> {
        match value.index {
            HEARTBEAT => {
                self.0.lock().unwrap().push(value.value);
                Ok(())
            }
            _ => Err(ExceptionCode::IllegalDataAddress),
        }
    }
}

async fn spawn_bms() -> Result<(SocketAddr, Arc<Mutex<Vec<u16>>>, ServerHandle)> {
    // Reason: bind-and-drop to learn a free port; rodbus takes a concrete addr.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let addr = listener.local_addr()?;
    drop(listener);
    let beats = Arc::new(Mutex::new(Vec::new()));
    let map = ServerHandlerMap::single(UnitId::new(UNIT), Bms(beats.clone()).wrap());
    let server =
        spawn_tcp_server_task(4, addr, map, AddressFilter::Any, DecodeLevel::default()).await?;
    Ok((addr, beats, server))
}

fn spec(bms: SocketAddr) -> Value {
    json!({
        "info": { "version": "v1" },
        "x-protocol-source": {},
        "x-heartbeat-source": { BMS: {
            "protocol": "modbus_tcp", "host": bms.ip().to_string(), "port": bms.port(),
            "unit_id": UNIT.to_string(), "address": HEARTBEAT, "data_type": "uint16",
            "function_code": 6, "scale": 1.0, "offset": 0.0,
            "counter_modulo": 3, "period_s": 0.2,
        }},
    })
}

fn config(device_api_url: String, broker_url: String) -> Config {
    unsafe {
        std::env::set_var("MQTT_GATEWAY_PASSWORD", "test");
    }
    Config {
        device_api_url,
        broker_url,
        mqtt_username: "arcnode_gateway".to_string(),
        site_id: "local_site".to_string(),
        log_level: "info".to_string(),
        gateway_credentials: None,
    }
}

#[tokio::test]
async fn the_counter_counts_up_and_wraps_at_its_modulo() -> Result<()> {
    // Arrange
    let hivemq = start_hivemq(&unique_network()).await?;
    let broker_url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let (addr, beats, _server) = spawn_bms().await?;
    let (stub, _locked) = spawn_lockable_stub(spec(addr)).await;
    let cancel = CancellationToken::new();
    let gateway = tokio::spawn(app::run(config(stub.uri(), broker_url), cancel.clone()));
    // Act
    timeout(Duration::from_secs(60), async {
        while beats.lock().unwrap().len() < 5 {
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await?;
    // Assert
    assert_eq!(beats.lock().unwrap()[..5], [0, 1, 2, 0, 1]);
    cancel.cancel();
    gateway.await??;
    Ok(())
}

#[tokio::test]
async fn a_locked_out_bms_gets_no_heartbeat_until_the_lock_clears() -> Result<()> {
    // Arrange — locked from boot
    let hivemq = start_hivemq(&unique_network()).await?;
    let broker_url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let (addr, beats, _server) = spawn_bms().await?;
    let (stub, locked) = spawn_lockable_stub(spec(addr)).await;
    *locked.lock().unwrap() = vec![BMS.to_string()];
    let cancel = CancellationToken::new();
    let gateway = tokio::spawn(app::run(
        config(stub.uri(), broker_url.clone()),
        cancel.clone(),
    ));
    sleep(Duration::from_secs(3)).await;
    assert_eq!(beats.lock().unwrap().len(), 0, "beat while locked");
    // Act — the lock clears and device-api beacons
    locked.lock().unwrap().clear();
    let op = paho_mqtt::AsyncClient::new(broker_url.as_str())?;
    op.connect(None).await?;
    op.publish(paho_mqtt::Message::new(
        "system/loto_changed",
        r#"{"ts":"t"}"#,
        1,
    ))
    .await?;
    // Assert — beats start
    timeout(Duration::from_secs(60), async {
        while beats.lock().unwrap().is_empty() {
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await?;
    cancel.cancel();
    gateway.await??;
    Ok(())
}
