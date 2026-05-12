//! BACnet/IP client. Reads `present_value` from a remote `AnalogInput`
//! object via a single `ReadProperty` request over UDP.

pub mod client;

pub use client::read_measurement;
