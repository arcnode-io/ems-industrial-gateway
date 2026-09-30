//! Modbus client + decode helpers.

pub mod client;
pub mod codec;
pub mod sunspec;
pub mod tls;
pub mod transport;

#[cfg(test)]
mod client_test;
#[cfg(test)]
mod codec_test;
#[cfg(test)]
mod sunspec_test;
