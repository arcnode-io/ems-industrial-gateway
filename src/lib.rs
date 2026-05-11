//! ems-industrial-gateway — Rust gateway translating south-side grid protocols
//! to north-side MQTT, driven by the AsyncAPI spec served by ems-device-api.

pub mod app;
pub mod asyncapi;
pub mod config;
pub mod http;
pub mod modbus;
pub mod mqtt;
