//! In-process plain-TCP DNP3 outstation seeded like the SEL-351's DNP map:
//! a phase voltage as an analog input (kV primary) and trip/fault targets
//! as binary inputs. Reuses the TLS fixture's outstation boilerplate.

use crate::fixtures::dnp3_security::{App, Ctl, Info, NopListener, outstation_config};
use anyhow::Result;
use dnp3::app::measurement::{AnalogInput, BinaryInput, Flags, Time};
use dnp3::link::LinkErrorMode;
use dnp3::outstation::database::{
    Add, AnalogInputConfig, BinaryInputConfig, EventAnalogInputVariation,
    EventBinaryInputVariation, EventClass, StaticAnalogInputVariation, StaticBinaryInputVariation,
    Update, UpdateOptions,
};
use dnp3::tcp::{AddressFilter, Server, ServerHandle};
use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::TcpListener;

/// SEL-351 AI 8: phase A voltage, kV primary.
pub const PHASE_VOLTAGE_A: u16 = 8;
/// Its seeded value, kV.
pub const PHASE_VOLTAGE_A_KV: f64 = 13.8;
/// SEL-351 BI 9: TRIP_LED, set.
pub const TRIP_STATUS: u16 = 9;
/// SEL-351 BI 15: G (ground) target, clear.
pub const GROUND_FAULT: u16 = 15;

/// Spawn on an OS-assigned loopback port; drop the handle to shut down.
pub async fn spawn() -> Result<(SocketAddr, ServerHandle)> {
    // Reason: bind-and-drop to learn a free port; the server takes a concrete addr.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let addr = listener.local_addr()?;
    drop(listener);
    let mut server = Server::new_tcp_server(LinkErrorMode::Close, addr);
    let outstation = server.add_outstation(
        outstation_config(),
        Box::new(App),
        Box::new(Info),
        Box::new(Ctl),
        Box::new(NopListener),
        AddressFilter::Any,
    )?;
    let now = Time::synchronized(0);
    outstation.transaction(|db| {
        db.add(
            PHASE_VOLTAGE_A,
            Some(EventClass::Class1),
            AnalogInputConfig {
                // Reason: a float static variation, so a master that forces
                // the 32-bit integer form (Var 1) would truncate 13.8 to 13.
                s_var: StaticAnalogInputVariation::Group30Var5,
                e_var: EventAnalogInputVariation::Group32Var5,
                deadband: 0.0,
            },
        );
        db.update(
            PHASE_VOLTAGE_A,
            &AnalogInput::new(PHASE_VOLTAGE_A_KV, Flags::ONLINE, now),
            UpdateOptions::default(),
        );
        for (index, value) in [(TRIP_STATUS, true), (GROUND_FAULT, false)] {
            db.add(
                index,
                Some(EventClass::Class1),
                BinaryInputConfig {
                    s_var: StaticBinaryInputVariation::Group1Var2,
                    e_var: EventBinaryInputVariation::Group2Var1,
                },
            );
            db.update(
                index,
                &BinaryInput::new(value, Flags::ONLINE, now),
                UpdateOptions::default(),
            );
        }
    });
    Ok((addr, server.bind().await?))
}
