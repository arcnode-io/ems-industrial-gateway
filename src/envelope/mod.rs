//! BESS envelope actuation: keeps a module's real Modbus setpoint within
//! `operating_envelope`'s live import/export limits, autonomously — the
//! only self-triggering write path in the gateway (everything else only
//! ever writes in direct response to an inbound MQTT command).
//!
//! See `control_law` for the pure ramp/hysteresis/clamp state machine and
//! `task` for the per-module tick loop + `distribute`-binding wire parsing.

mod anti_windup;
pub mod bounds;
pub mod config;
pub mod control_law;
#[cfg(test)]
mod control_law_servo_test;
#[cfg(test)]
mod control_law_step_test;
#[cfg(test)]
mod control_law_test;
pub mod inputs;
pub mod poi_gate;
#[cfg(test)]
mod poi_gate_test;
pub mod poi_servo;
#[cfg(test)]
mod poi_servo_test;
pub mod shed;
pub mod shed_task;
pub mod storage_spare;
pub mod task;
pub mod writes;

pub use config::{EnvelopeGuardConfig, EnvelopeTaskConfig, envelope_guard_config};
