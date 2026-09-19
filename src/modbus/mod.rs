//! Modbus client + decode helpers.

pub mod client;
pub mod codec;
pub mod tls;

#[cfg(test)]
mod client_test;
#[cfg(test)]
mod codec_test;
