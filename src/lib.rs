//! ems-industrial-gateway — Rust gateway translating south-side grid protocols
//! to north-side MQTT, driven by the AsyncAPI spec served by ems-device-api.

pub mod app;
pub mod asyncapi;
pub mod config;
pub mod dnp3;
pub mod http;
pub mod modbus;
pub mod mqtt;
pub mod redfish;
pub mod snmp;
