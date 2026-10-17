//! SunSpec over Modbus TCP: find a device's models at connect, resolve a
//! bound point (model + point id) to its registers, read and write it.

pub mod bits;
pub mod client;
pub mod discover;
pub mod heartbeat;
pub mod models;
pub mod resolve;
pub mod write;

#[cfg(test)]
mod resolve_test;
