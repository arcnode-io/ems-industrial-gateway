//! Bindings with no south-side device: synthetic (computed from cached MQTT
//! inputs) and distribute (a command split across child devices).

use serde::Deserialize;

/// Synthetic binding: gateway computes a value from cached MQTT inputs via a
/// named operation. No south-side device; the "south" is MQTT itself.
///
/// Topic placeholders in `inputs`:
/// - `{site_id}` — substituted from gateway runtime config at subscribe time.
/// - `{device_id}` — already resolved by `ems-device-api` at AsyncAPI gen time.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyntheticBinding {
    /// Operation name: `subtract`, `sum`, `mean`, `max`, `min`, `weighted_mean`.
    pub operation: String,
    /// Input topic templates the synthetic task subscribes to and caches.
    /// Present for `subtract`/`sum`/`mean`/`max`/`min`; absent (empty) when
    /// `operation` is `weighted_mean`, which uses `pairs` instead.
    #[serde(default)]
    pub inputs: Vec<String>,
    /// `(topic, weight)` pairs — only present when `operation` is
    /// `weighted_mean`. Device-api resolves the weight (e.g. a child rack's
    /// `capacity_kwh`) at spec-build time; the gateway just reads the named
    /// topics and applies `synthetic::operation::weighted_mean`.
    #[serde(default)]
    pub pairs: Vec<WeightedPair>,
}

/// One `(topic, weight)` entry in a `weighted_mean` synthetic binding.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeightedPair {
    /// The MQTT topic to read the value from.
    pub topic: String,
    /// The weight to apply to that topic's value.
    pub weight: f64,
}

/// Command-distribution binding: a `bess_module`-style command splits one
/// setpoint across N children (e.g. `bess_rack` instances) via a max-min
/// fair allocation policy. See `dispatch::allocation`.
///
/// The eight fields from `ramp_rate_per_sec` to `active_power_topic` are
/// envelope-guard config — all present together, or all absent, per
/// `envelope::envelope_guard_config`. A plain distribute binding (no
/// envelope guard) has none of them; nothing about a device's own
/// `set_active_power` command changes shape depending on whether it's
/// envelope-guarded.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistributeBinding {
    /// Allocation policy name: `equal_split` or `soc_weighted`.
    pub allocation_policy: String,
    /// Fully-resolved children to distribute the setpoint across.
    pub children: Vec<ChildAllocation>,
    /// Reserve floor, percent of each child's own state_of_charge (0-100).
    /// A child at or below it gets no discharge share; charging is never
    /// restricted. Resolved by ems-device-api from the DTM's
    /// `bess_reserve_floor_mwh` over site-wide rack capacity; absent = no floor.
    #[serde(default)]
    pub state_of_charge_floor_percent: Option<f64>,
    /// Ramp rate on recovery, as a fraction of rated power per second.
    #[serde(default)]
    pub ramp_rate_per_sec: Option<f64>,
    /// Required headroom margin to close a constrained event, as a
    /// fraction of rated power.
    #[serde(default)]
    pub hysteresis_margin: Option<f64>,
    /// How long that margin must hold continuously before closing the event.
    #[serde(default)]
    pub hysteresis_dwell_secs: Option<f64>,
    /// The module's own static lower bound (`active_power.bounds.min`).
    #[serde(default)]
    pub power_min: Option<f64>,
    /// The module's own static upper bound (`active_power.bounds.max`).
    #[serde(default)]
    pub power_max: Option<f64>,
    /// MQTT topic carrying the live `operating_envelope.import_limit`.
    #[serde(default)]
    pub import_limit_topic: Option<String>,
    /// MQTT topic carrying the live `operating_envelope.export_limit`.
    #[serde(default)]
    pub export_limit_topic: Option<String>,
    /// MQTT topic carrying the module's own live `active_power` reading.
    #[serde(default)]
    pub active_power_topic: Option<String>,
    /// MQTT topic carrying the POI meter's `active_power` (+ import), so the
    /// envelope bounds site net power rather than the battery alone. Optional
    /// and outside the all-or-nothing guard set: absent means site load 0.
    #[serde(default)]
    pub poi_active_power_topic: Option<String>,
}

/// One child's identity + static bounds for command distribution.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildAllocation {
    /// The child device's id — resolves its own write binding via the same
    /// (device_id, verb+target) lookup the module's own command used.
    pub device_id: String,
    /// MQTT topic carrying the child's cached `operating_state` reading —
    /// `FAULT`/`OFFLINE` exclude it from allocation.
    pub operating_state_topic: String,
    /// MQTT topic carrying the child's cached `state_of_charge` reading —
    /// only read for the `soc_weighted` policy.
    pub state_of_charge_topic: String,
    /// The child's own static lower bound (e.g. `active_power.bounds.min`).
    pub power_min: f64,
    /// The child's own static upper bound (e.g. `active_power.bounds.max`).
    pub power_max: f64,
}
