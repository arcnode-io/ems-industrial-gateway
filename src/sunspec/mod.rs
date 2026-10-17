//! SunSpec over Modbus TCP: find a device's models at connect, resolve a
//! bound point (model + point id) to its registers, read and write it.

pub mod client;
pub mod discover;
pub mod models;
pub mod resolve;

#[cfg(test)]
mod resolve_test;
