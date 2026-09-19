//! BESS envelope actuation: keeps a module's real Modbus setpoint within
//! `operating_envelope`'s live import/export limits, autonomously — the
//! only self-triggering write path in the gateway (everything else only
//! ever writes in direct response to an inbound MQTT command).
//!
//! See `control_law` for the pure ramp/hysteresis/clamp state machine.

pub mod control_law;
#[cfg(test)]
mod control_law_test;
