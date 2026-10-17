//! e2e: the EPC CmdBits heartbeat. The PCS drops out unless CmdBits is
//! rewritten at least once a second, so the gateway feeds it, but only
//! while the EMS can steer: the bits it binds start at 0 (stopped until
//! enabled), commands change one bit, a trigger is written once, and the
//! beat stops when the broker goes or the device is locked out.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::{app, config::Config};
use fixtures::containers::{start_hivemq, unique_network};
use fixtures::pcs_operator::{command, measurement, operator};
use fixtures::spec_stub::spawn_lockable_stub;
use fixtures::sunspec_pcs::{Pcs, at, spawn};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::{Instant, sleep, timeout};
use tokio_util::sync::CancellationToken;

const SITE_ID: &str = "local_site";
const PCS: &str = "pcs_1";
/// Set at commissioning; the gateway binds no command to it.
const VSENSE_REM: u16 = 0b100;

fn spec(pcs: &Pcs) -> Value {
    let at = |extra: Value| {
        let mut entry = json!({
            "protocol": "sunspec", "host": pcs.addr.ip().to_string(),
            "port": pcs.addr.port(), "unit_id": "1",
        });
        entry
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        entry
    };
    let bit = |verb: &str, target: &str, bit: u8| {
        at(json!({ "verb": verb, "target": target, "unit": "none",
                   "model_id": 65534, "point": "CmdBits", "bit": bit }))
    };
    json!({
        "info": { "version": "v1" },
        "x-protocol-source": { PCS: {
            "active_power": at(json!({ "unit": "watts", "poll_rate_hz": 1.0,
                                       "model_id": 103, "point": "W" })),
        }},
        "x-command-source": { PCS: {
            "enable_run": bit("enable", "run", 0),
            "disable_run": bit("disable", "run", 0),
            "clear_faults": bit("clear", "faults", 1),
            "set_active_power": at(json!({ "verb": "set", "target": "active_power",
                "unit": "watts", "model_id": 65534, "point": "CmdRealPwr" })),
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
        site_id: SITE_ID.to_string(),
        log_level: "info".to_string(),
        gateway_credentials: None,
    }
}

/// The low words of every CmdBits write so far.
fn beats(pcs: &Pcs) -> Vec<u16> {
    pcs.writes_at(at::CMD_BITS)
        .iter()
        .map(|w| w.words[1])
        .collect()
}

/// Wait until the PCS has taken `n` more CmdBits writes.
async fn more_beats(pcs: &Pcs, n: usize) -> Result<()> {
    let from = beats(pcs).len();
    timeout(Duration::from_secs(30), async {
        while beats(pcs).len() < from + n {
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn the_heartbeat_holds_the_pcs_stopped_until_the_ems_enables_it() -> Result<()> {
    // Arrange
    let hivemq = start_hivemq(&unique_network()).await?;
    let broker_url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let pcs = spawn(40000).await?;
    pcs.set(at::CMD_BITS + 1, VSENSE_REM | 0b1);
    pcs.set(at::W, 1234);
    pcs.set(at::W_SF, 0xFFFF);
    let (stub, _locked) = spawn_lockable_stub(spec(&pcs)).await;
    let (op, mut inbox) = operator(&broker_url).await?;
    let cancel = CancellationToken::new();
    let gateway = tokio::spawn(app::run(config(stub.uri(), broker_url), cancel.clone()));

    // Assert — beats at least once a second, En cleared, VsenseRem kept
    more_beats(&pcs, 4).await?;
    assert!(
        beats(&pcs).iter().all(|&w| w == VSENSE_REM),
        "{:?}",
        beats(&pcs)
    );
    let times: Vec<Instant> = pcs.writes_at(at::CMD_BITS).iter().map(|w| w.at).collect();
    let gap = times.windows(2).map(|t| t[1] - t[0]).max().unwrap();
    assert!(gap < Duration::from_secs(1), "longest gap {gap:?}");
    // ...and the inverter's power reads through W_SF
    let watts = measurement(&mut inbox).await?;
    assert!((watts - 123.4).abs() < 1e-9, "read {watts} W");

    // Act + Assert — enable latches En
    assert_eq!(
        command(&op, &mut inbox, "enable/run/none", "e1", 1.0).await?,
        "done"
    );
    more_beats(&pcs, 2).await?;
    assert_eq!(beats(&pcs).last(), Some(&(VSENSE_REM | 0b1)));

    // Act + Assert — clearing faults writes FltClr once; the next beat drops it
    assert_eq!(
        command(&op, &mut inbox, "clear/faults/none", "c1", 1.0).await?,
        "done"
    );
    more_beats(&pcs, 2).await?;
    assert!(beats(&pcs).contains(&(VSENSE_REM | 0b11)));
    assert_eq!(beats(&pcs).last(), Some(&(VSENSE_REM | 0b1)));

    // Act + Assert — a setpoint lands as an int32 in PWR_SF units (sf 0)
    let set = "set/active_power/watts";
    assert_eq!(command(&op, &mut inbox, set, "p1", 50_000.0).await?, "done");
    let written = pcs.writes_at(at::CMD_REAL_PWR);
    assert_eq!(
        written.last().map(|w| w.words.clone()),
        Some(vec![0, 50_000])
    );

    cancel.cancel();
    gateway.await??;
    Ok(())
}

#[tokio::test]
async fn the_heartbeat_stops_when_the_broker_goes() -> Result<()> {
    // Arrange — beating
    let hivemq = start_hivemq(&unique_network()).await?;
    let broker_url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let pcs = spawn(40000).await?;
    let (stub, _locked) = spawn_lockable_stub(spec(&pcs)).await;
    let cancel = CancellationToken::new();
    let _gateway = tokio::spawn(app::run(config(stub.uri(), broker_url), cancel.clone()));
    more_beats(&pcs, 2).await?;
    // Act
    hivemq.stop().await?;
    sleep(Duration::from_secs(2)).await;
    let after_stop = beats(&pcs).len();
    sleep(Duration::from_secs(3)).await;
    // Assert — the PCS's watchdog is left to trip it
    assert_eq!(
        beats(&pcs).len(),
        after_stop,
        "still beating with no broker"
    );
    cancel.cancel();
    Ok(())
}

#[tokio::test]
async fn a_locked_out_pcs_gets_no_heartbeat() -> Result<()> {
    // Arrange — pcs_1 locked out from boot
    let hivemq = start_hivemq(&unique_network()).await?;
    let broker_url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let pcs = spawn(40000).await?;
    pcs.set(at::W_SF, 0);
    let (stub, locked) = spawn_lockable_stub(spec(&pcs)).await;
    *locked.lock().unwrap() = vec![PCS.to_string()];
    let (_op, mut inbox) = operator(&broker_url).await?;
    let cancel = CancellationToken::new();
    let gateway = tokio::spawn(app::run(config(stub.uri(), broker_url), cancel.clone()));
    // Act — the gateway is up (it publishes readings) for a few beats' time
    measurement(&mut inbox).await?;
    sleep(Duration::from_secs(2)).await;
    // Assert
    assert!(beats(&pcs).is_empty(), "{:?}", beats(&pcs));
    cancel.cancel();
    gateway.await??;
    Ok(())
}
