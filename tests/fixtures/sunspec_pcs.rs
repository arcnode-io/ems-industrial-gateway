//! An in-process EPC CAB1000-style PCS: a SunSpec map of models
//! 1 → 103 → 65534 over Modbus TCP. Addresses come from EPC's SMDX files
//! laid out from `base` (40000 on a stock unit; W lands at 40084), not from
//! the gateway's own tables, so a wrong offset there fails here.

use anyhow::Result;
use rodbus::server::{
    AddressFilter, RequestHandler, ServerHandle, ServerHandlerMap, spawn_tcp_server_task,
};
use rodbus::{DecodeLevel, ExceptionCode, Indexed, UnitId};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::time::Instant;

/// Each register's offset from the SunSpec base on a stock unit.
pub mod at {
    pub const W: u16 = 84;
    pub const W_SF: u16 = 85;
    pub const HZ: u16 = 86;
    pub const ST: u16 = 108;
    pub const CMD_BITS: u16 = 124;
    pub const CMD_REAL_PWR: u16 = 126;
    pub const CMD_REACTIVE_PWR: u16 = 128;
    pub const PWR_SF: u16 = 130;
    pub const SYS_FLT: u16 = 143;
}

/// One register write the PCS took: when, where, what.
#[derive(Debug, Clone)]
pub struct Write {
    pub at: Instant,
    pub address: u16,
    pub words: Vec<u16>,
}

/// The PCS's registers and the writes it has taken, shared with the test.
#[derive(Default)]
pub struct Registers {
    pub words: HashMap<u16, u16>,
    pub writes: Vec<Write>,
}

/// A running PCS. `base` is where its `SunS` marker sits.
pub struct Pcs {
    pub addr: SocketAddr,
    pub base: u16,
    pub registers: Arc<Mutex<Registers>>,
    _server: ServerHandle,
}

impl Pcs {
    /// Set the register `offset` past the base.
    pub fn set(&self, offset: u16, word: u16) {
        let mut regs = self.registers.lock().unwrap();
        regs.words.insert(self.base + offset, word);
    }

    /// The writes taken at `offset` past the base.
    pub fn writes_at(&self, offset: u16) -> Vec<Write> {
        let regs = self.registers.lock().unwrap();
        let address = self.base + offset;
        regs.writes
            .iter()
            .filter(|w| w.address == address)
            .cloned()
            .collect()
    }
}

struct Handler(Arc<Mutex<Registers>>);

impl RequestHandler for Handler {
    fn read_holding_register(&self, address: u16) -> Result<u16, ExceptionCode> {
        let regs = self.0.lock().unwrap();
        regs.words
            .get(&address)
            .copied()
            .ok_or(ExceptionCode::IllegalDataAddress)
    }

    fn write_multiple_registers(
        &mut self,
        values: rodbus::server::WriteRegisters,
    ) -> Result<(), ExceptionCode> {
        let mut regs = self.0.lock().unwrap();
        let words: Vec<Indexed<u16>> = values.iterator.collect();
        for w in &words {
            regs.words.insert(w.index, w.value);
        }
        let write = Write {
            at: Instant::now(),
            address: values.range.start,
            words: words.iter().map(|w| w.value).collect(),
        };
        regs.writes.push(write);
        Ok(())
    }
}

/// The model chain from `base`: SunS, then each model's ID/L header and
/// its zeroed points, then the 0xFFFF end marker.
fn chain(base: u16) -> HashMap<u16, u16> {
    let mut words = HashMap::from([(base, 0x5375), (base + 1, 0x6e53)]);
    let mut addr = base + 2;
    for (id, len) in [(1u16, 66u16), (103, 50), (65534, 44)] {
        words.insert(addr, id);
        words.insert(addr + 1, len);
        for i in 0..len {
            words.insert(addr + 2 + i, 0);
        }
        addr += 2 + len;
    }
    words.insert(addr, 0xFFFF);
    words.insert(addr + 1, 0);
    words
}

/// Start a PCS whose SunSpec map begins at `base`, on unit id 1.
pub async fn spawn(base: u16) -> Result<Pcs> {
    // Reason: bind-and-drop to learn a free port; rodbus takes a concrete addr.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let addr = listener.local_addr()?;
    drop(listener);
    let registers = Arc::new(Mutex::new(Registers {
        words: chain(base),
        writes: Vec::new(),
    }));
    let handler = Handler(registers.clone()).wrap();
    let map = ServerHandlerMap::single(UnitId::new(1), handler);
    let server =
        spawn_tcp_server_task(4, addr, map, AddressFilter::Any, DecodeLevel::default()).await?;
    Ok(Pcs {
        addr,
        base,
        registers,
        _server: server,
    })
}
