//! Unit tests for the pure module-discovery helper. High-risk: silently
//! skipping a real module (missing bounds, or treating a rack-level device
//! as a module) would under-allocate site power without any visible error.

use super::modules_with_bounds;
use crate::asyncapi::types::{DistributeBinding, ModbusTcpBinding, ProtocolBinding};
use crate::modbus::client::{ModbusDataType, WordOrder};
use crate::synthetic::new_input_cache;
use std::collections::HashMap;
use std::time::Instant;

const SITE_ID: &str = "site_001";

fn distribute_binding(power_min: f64, power_max: f64) -> ProtocolBinding {
    ProtocolBinding::Distribute(DistributeBinding {
        allocation_policy: "soc_weighted".to_string(),
        children: vec![],
        ramp_rate_per_sec: None,
        hysteresis_margin: None,
        hysteresis_dwell_secs: None,
        power_min: Some(power_min),
        power_max: Some(power_max),
        import_limit_topic: None,
        export_limit_topic: None,
        active_power_topic: None,
    })
}

fn modbus_binding() -> ProtocolBinding {
    ProtocolBinding::ModbusTcp(ModbusTcpBinding {
        host: "127.0.0.1".to_string(),
        port: 502,
        unit_id: "1".to_string(),
        address: 50,
        scale: 1.0,
        offset: 0.0,
        data_type: ModbusDataType::Int32,
        word_order: WordOrder::HighLow,
    })
}

#[test]
fn discovers_modules_with_distribute_bindings_and_cached_soc() {
    // Arrange
    let cache = new_input_cache();
    cache.insert(
        "sites/site_001/devices/bess_module_1/measurements/state_of_charge/percent".to_string(),
        (70.0, Instant::now()),
    );
    let mut commands = HashMap::new();
    commands.insert(
        "set_active_power".to_string(),
        distribute_binding(-8_000_000.0, 8_000_000.0),
    );
    let mut channels = HashMap::new();
    channels.insert("bess_module_1".to_string(), commands);

    // Act
    let modules = modules_with_bounds(&channels, &cache, SITE_ID, 400_000.0).unwrap();

    // Assert
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].device_id, "bess_module_1");
    assert_eq!(modules[0].headroom, 8_000_000.0);
    assert!((modules[0].state_of_charge - 70.0).abs() < f64::EPSILON);
}

#[test]
fn headroom_is_direction_dependent_on_target_sign() {
    // Arrange — charging (negative target) uses |power_min|, not power_max.
    let cache = new_input_cache();
    cache.insert(
        "sites/site_001/devices/bess_module_1/measurements/state_of_charge/percent".to_string(),
        (50.0, Instant::now()),
    );
    let mut commands = HashMap::new();
    commands.insert(
        "set_active_power".to_string(),
        distribute_binding(-8_000_000.0, 4_000_000.0),
    );
    let mut channels = HashMap::new();
    channels.insert("bess_module_1".to_string(), commands);

    // Act
    let modules = modules_with_bounds(&channels, &cache, SITE_ID, -100_000.0).unwrap();

    // Assert
    assert_eq!(modules[0].headroom, 8_000_000.0);
}

#[test]
fn holds_when_a_known_modules_soc_is_not_cached() {
    // Arrange — no cache entry at all for bess_module_1.
    let cache = new_input_cache();
    let mut commands = HashMap::new();
    commands.insert(
        "set_active_power".to_string(),
        distribute_binding(-8_000_000.0, 8_000_000.0),
    );
    let mut channels = HashMap::new();
    channels.insert("bess_module_1".to_string(), commands);

    // Act
    let modules = modules_with_bounds(&channels, &cache, SITE_ID, 400_000.0);

    // Assert
    assert!(modules.is_none());
}

#[test]
fn skips_devices_without_a_distribute_bound_set_active_power_command() {
    // Arrange — a rack-level device with a plain ModbusTcp command, not
    // a distribute parent.
    let cache = new_input_cache();
    let mut commands = HashMap::new();
    commands.insert("set_active_power".to_string(), modbus_binding());
    let mut channels = HashMap::new();
    channels.insert("rack_1".to_string(), commands);

    // Act
    let modules = modules_with_bounds(&channels, &cache, SITE_ID, 400_000.0).unwrap();

    // Assert
    assert!(modules.is_empty());
}

#[test]
fn empty_channels_yields_no_modules() {
    let cache = new_input_cache();
    let channels = HashMap::new();
    let modules = modules_with_bounds(&channels, &cache, SITE_ID, 400_000.0).unwrap();
    assert!(modules.is_empty());
}
