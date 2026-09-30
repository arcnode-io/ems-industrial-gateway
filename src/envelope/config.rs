//! Wire parsing of a `distribute` binding into what an envelope task runs on.

use crate::asyncapi::types::{DistributeBinding, ProtocolBinding};
use crate::envelope::control_law::EnvelopeConfig;
use std::time::Duration;

/// Resolved envelope-guard config for one `distribute` binding — `Some`
/// only when every guard field is present; a plain (non-guarded) distribute
/// binding has none of them. Topic fields are the raw (still-templated)
/// wire values; the caller substitutes `{site_id}` when building
/// `EnvelopeTaskConfig`, same as `app.rs` already does for synthetic inputs.
pub struct EnvelopeGuardConfig {
    /// `EnvelopeController`'s ramp/hysteresis parameters.
    pub control: EnvelopeConfig,
    /// The module's own static lower bound (`active_power.bounds.min`).
    pub power_min: f64,
    /// The module's own static upper bound (`active_power.bounds.max`).
    pub power_max: f64,
    /// MQTT topic template carrying the live `import_limit`.
    pub import_limit_topic: String,
    /// MQTT topic template carrying the live `export_limit`.
    pub export_limit_topic: String,
    /// MQTT topic template carrying the module's own live `active_power`.
    pub active_power_topic: String,
    /// MQTT topic template carrying the POI meter's `active_power`; `None`
    /// means no POI meter, so site load is taken as 0.
    pub poi_active_power_topic: Option<String>,
}

/// Extract envelope-guard config from a `distribute` binding. `None` if any
/// guard field is missing — the binding is a plain, unguarded distribute.
pub fn envelope_guard_config(d: &DistributeBinding) -> Option<EnvelopeGuardConfig> {
    Some(EnvelopeGuardConfig {
        control: EnvelopeConfig {
            ramp_rate_per_sec: d.ramp_rate_per_sec?,
            hysteresis_margin: d.hysteresis_margin?,
            hysteresis_dwell: Duration::from_secs_f64(d.hysteresis_dwell_secs?),
        },
        power_min: d.power_min?,
        power_max: d.power_max?,
        import_limit_topic: d.import_limit_topic.clone()?,
        export_limit_topic: d.export_limit_topic.clone()?,
        active_power_topic: d.active_power_topic.clone()?,
        poi_active_power_topic: d.poi_active_power_topic.clone(),
    })
}

/// Everything one rebalance/envelope task needs to run forever.
pub struct EnvelopeTaskConfig {
    /// The device (module, or a future site-level virtual parent) this task
    /// distributes for.
    pub device_id: String,
    /// `{verb}_{target}` key into `device_channels`/`last_requested`.
    pub channel_key: String,
    /// `Some` for an envelope-guarded distribute (headroom clamp applies);
    /// `None` for a plain distribute — every tick still rebalances the
    /// child split, just with no clamp on top.
    pub guard: Option<EnvelopeGuardConfig>,
    /// The distribute binding this task rebalances. Must be
    /// `ProtocolBinding::Distribute` — the only kind of binding this task
    /// is ever spawned for.
    pub binding: ProtocolBinding,
    /// Tick cadence — matches the module's `active_power` poll rate (1 Hz).
    pub tick_hz: f64,
}
