//! `x-heartbeat-source`: per device, a register the EMS must keep writing
//! or the device faults (CATL's BMS raises "EMS heartbeat fault" and may
//! open its contactors). The template declares it; device-api merges the
//! device's connection in, as for commands.

use super::ProtocolBinding;
use crate::modbus::function_code::write_function;
use serde::Deserialize;
use serde::de::Deserializer;
use std::collections::HashMap;
use tracing::warn;

/// One device's heartbeat: write 0, 1, … `counter_modulo`-1, 0, … once a
/// `period_s`.
#[derive(Debug, Deserialize)]
pub struct HeartbeatSource {
    /// The counter wraps to 0 here; above 1.
    pub counter_modulo: u16,
    /// Seconds between writes; above 0.
    pub period_s: f64,
    /// Where to write it. Modbus TCP only, today.
    #[serde(flatten)]
    pub binding: ProtocolBinding,
}

/// `x-heartbeat-source` with unrunnable entries dropped, loudly: a device
/// whose heartbeat the gateway can't run will fault, and the log says why.
pub fn heartbeats<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<HashMap<String, HeartbeatSource>, D::Error> {
    let all = HashMap::<String, HeartbeatSource>::deserialize(d)?;
    Ok(all
        .into_iter()
        .filter(|(device_id, h)| match runnable(h) {
            Ok(()) => true,
            Err(reason) => {
                warn!(%device_id, %reason, "heartbeat can't run; the device will fault");
                false
            }
        })
        .collect())
}

/// Why `h` can't run, if it can't.
fn runnable(h: &HeartbeatSource) -> Result<(), String> {
    if h.counter_modulo < 2 {
        return Err(format!("counter_modulo {} doesn't count", h.counter_modulo));
    }
    if !(h.period_s > 0.0 && h.period_s.is_finite()) {
        return Err(format!("period_s {} isn't a period", h.period_s));
    }
    match &h.binding {
        ProtocolBinding::ModbusTcp(m) => write_function(m.function_code, m.data_type).map(drop),
        _ => Err("only modbus_tcp heartbeats are supported".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use crate::asyncapi::types::AsyncApiSpec;
    use serde_json::json;

    #[test]
    fn a_heartbeat_the_gateway_cant_run_is_dropped() {
        // Arrange — a good counter, one that never counts, one on FC3 (a read)
        let beat = |modulo: u16, fc: u8| {
            json!({
                "protocol": "modbus_tcp", "host": "10.0.0.9", "port": 502, "unit_id": "100",
                "address": 896, "data_type": "uint16", "function_code": fc,
                "scale": 1.0, "offset": 0.0, "counter_modulo": modulo, "period_s": 1.0,
            })
        };
        let spec = json!({
            "info": { "version": "v1" }, "x-protocol-source": {},
            "x-heartbeat-source": { "ok": beat(256, 6), "stuck": beat(1, 6), "read": beat(256, 3) },
        });
        // Act
        let parsed: AsyncApiSpec = serde_json::from_value(spec).unwrap();
        // Assert
        let kept: Vec<&String> = parsed.x_heartbeat_source.keys().collect();
        assert_eq!(kept, ["ok"]);
    }
}
