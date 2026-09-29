//! Envelope limits referenced to the POI, not the battery. Site load `L`
//! (POI import + battery discharge) shifts both bounds: discharge may reach
//! `L + export_limit`, charge may reach `L − import_limit`.

use super::control_law::{EnvelopeConfig, EnvelopeController, EnvelopeTick};
use std::time::Duration;

fn config() -> EnvelopeConfig {
    EnvelopeConfig {
        ramp_rate_per_sec: 0.10,
        hysteresis_margin: 0.05,
        hysteresis_dwell: Duration::from_secs(30),
    }
}

/// 72.8 kW compute load, the demo's sizing.
const SITE_LOAD: f64 = 72_800.0;

fn tick(
    import_limit: Option<f64>,
    export_limit: Option<f64>,
    active_power: f64,
    requested_setpoint: f64,
    site_load: f64,
) -> EnvelopeTick {
    EnvelopeTick {
        import_limit,
        export_limit,
        active_power,
        requested_setpoint,
        site_load,
        power_min: -4_000_000.0,
        power_max: 4_000_000.0,
        dt: Duration::from_secs(1),
    }
}

#[test]
fn zero_export_still_allows_discharge_up_to_site_load() {
    // Arrange — the live demo case: export_limit 0, 72.8 kW of site load.
    let mut ctrl = EnvelopeController::new(config(), 0.0);
    // Act — discharge 50 kW, less than the load, so the POI still imports
    let out = ctrl.tick(tick(Some(5_378_000.0), Some(0.0), 0.0, 50_000.0, SITE_LOAD));
    // Assert — nothing exported, so nothing to clamp
    assert_eq!(out, Some(50_000.0));
}

#[test]
fn zero_export_caps_discharge_at_site_load() {
    // Arrange — discharging exactly the load: POI at 0, export headroom 0.
    // Controller last wrote 50 kW, so a clamp to the load is a real change.
    let mut ctrl = EnvelopeController::new(config(), 50_000.0);
    // Act — ask for more than the load
    let out = ctrl.tick(tick(None, Some(0.0), SITE_LOAD, 100_000.0, SITE_LOAD));
    // Assert — capped where the POI would start exporting
    assert_eq!(out, Some(SITE_LOAD));
}

#[test]
fn import_limit_below_site_load_caps_charging_harder() {
    // Arrange — 100 kW import limit, 72.8 kW already imported by the load:
    // only 27.2 kW of charging fits before the POI exceeds its limit.
    let mut ctrl = EnvelopeController::new(config(), 0.0);
    // Act — charge 50 kW
    let out = ctrl.tick(tick(Some(100_000.0), None, -27_200.0, -50_000.0, SITE_LOAD));
    // Assert — floor = L − import_limit = −27.2 kW
    assert_eq!(out, Some(-27_200.0));
}

#[test]
fn zero_site_load_is_the_battery_only_law() {
    // Arrange — no POI meter: L = 0, export_limit caps discharge directly
    let mut ctrl = EnvelopeController::new(config(), 500_000.0);
    // Act
    let out = ctrl.tick(tick(None, Some(1_000_000.0), 1_000_000.0, 1_500_000.0, 0.0));
    // Assert
    assert_eq!(out, Some(1_000_000.0));
}
