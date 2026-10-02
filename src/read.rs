//! One read of one measurement, whatever its protocol.

use crate::asyncapi::trust::DeviceTrust;
use crate::asyncapi::types::ProtocolBinding;
use crate::bacnet::client as bacnet;
use crate::bacnet_sc::client as bacnet_sc;
use crate::config::GatewayCredentials;
use crate::dnp3::client as dnp3;
use crate::modbus::client as modbus;
use crate::payload::Raw;
use crate::redfish::client as redfish;
use crate::snmp::client as snmp;
use anyhow::Result;

/// Single-point protocol dispatch. Add a `match` arm when a new
/// `ProtocolBinding` variant lands.
pub async fn read_value(
    binding: &ProtocolBinding,
    trust: Option<&DeviceTrust>,
    creds: Option<&GatewayCredentials>,
) -> Result<Raw> {
    match binding {
        ProtocolBinding::ModbusTcp(b) => modbus::read_measurement(b, trust, creds)
            .await
            .map(Raw::Number),
        ProtocolBinding::Snmp(b) => snmp::read_measurement(b, trust, creds)
            .await
            .map(Raw::Number),
        ProtocolBinding::Redfish(b) => redfish::read_measurement(b, trust, creds).await,
        ProtocolBinding::Dnp3Tcp(b) => dnp3::read_measurement(b, trust, creds)
            .await
            .map(Raw::Number),
        ProtocolBinding::BacnetIp(b) => bacnet::read_measurement(b, trust, creds)
            .await
            .map(Raw::Number),
        ProtocolBinding::BacnetSc(b) => bacnet_sc::read_measurement(b, trust, creds)
            .await
            .map(Raw::Number),
        // Synthetic channels are driven by `src/synthetic/`; the poller is
        // never handed one. Unreachable is a tripwire for that routing.
        ProtocolBinding::Synthetic(_) => {
            unreachable!("synthetic bindings are driven by the synthetic module, not the poller")
        }
        // Distribute is command-only; it never appears in x-protocol-source.
        ProtocolBinding::Distribute(_) => {
            unreachable!("distribute bindings are commands, never a measurement source")
        }
    }
}
