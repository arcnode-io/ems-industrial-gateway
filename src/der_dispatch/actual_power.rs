//! Publishes `der_dispatch/actual_active_power` — site-level real delivered
//! power, summed across whatever distribute-parent devices this deployment's
//! topology currently has (today: `bess_module` instances; the sum is
//! correct at any count, including future multi-module sites, per handoff-
//! phase3-bess-command-distribution.md's scope).
//!
//! `der_dispatch` has no `contains:` relationship to those devices — it's a
//! sibling leaf in the DTM, not their parent — so this can't be expressed as
//! a `synthetic` binding (`source_measurement` only projects across a
//! declared parent's children; `inputs` is a fixed list, wrong for a
//! deployment-variable module count). Gateway-computed, published directly
//! to the fixed topic instead, the same way ems-der-control-api already
//! publishes `der_dispatch/target_active_power` with no protocol binding at
//! all. Open question for a real schema-level aggregation mode, tracked with
//! power-engineer — not blocking, since this works today regardless.

use crate::synthetic::InputCache;
use chrono::Utc;
use paho_mqtt::{AsyncClient, Message};
use std::time::Duration;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// QoS for the actual-side channel — matches der-control-api's own
/// target-side publish (DispatchPublisher.java: QOS=0, RETAIN=true).
const QOS_MEASUREMENT: i32 = 0;

/// Everything the der_dispatch publisher task needs to run forever.
pub struct DerDispatchTaskConfig {
    /// `sites/{site}/devices/der_dispatch/measurements/actual_active_power/watts`.
    pub output_topic: String,
    /// Each distribute-parent's own `active_power` topic (already
    /// `{site_id}`-substituted) to sum on every tick.
    pub source_topics: Vec<String>,
    /// Tick cadence — matches the module `active_power` poll rate (1 Hz).
    pub tick_hz: f64,
}

/// Spawn the site-level publisher loop. Mirrors `synthetic::task::spawn`'s
/// shutdown contract: the returned `JoinHandle` exits when `cancel` fires.
pub fn spawn(
    cfg: DerDispatchTaskConfig,
    cache: InputCache,
    mqtt: AsyncClient,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let period_ms = (1000.0 / cfg.tick_hz).max(1.0) as u64;
        let mut ticker = interval(Duration::from_millis(period_ms));
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = ticker.tick() => tick_once(&cfg, &cache, &mqtt).await,
            }
        }
    })
}

/// One tick: sum every source topic's cached value and publish. Holds (no
/// publish) until every source is cached — same posture as a synthetic
/// task's hold semantic. No source topics at all (a site with zero
/// distribute-parent devices) also holds — nothing to report yet.
async fn tick_once(cfg: &DerDispatchTaskConfig, cache: &InputCache, mqtt: &AsyncClient) {
    let Some(total) = sum_cached(&cfg.source_topics, cache) else {
        debug!(topic = %cfg.output_topic, "der_dispatch hold: not every DER asset's active_power cached yet");
        return;
    };
    let payload = format!(
        r#"{{"ts":"{ts}","value":{total}}}"#,
        ts = Utc::now().to_rfc3339(),
    );
    let msg = Message::new_retained(&cfg.output_topic, payload.into_bytes(), QOS_MEASUREMENT);
    if let Err(err) = mqtt.publish(msg).await {
        warn!(topic = %cfg.output_topic, error = %err, "der_dispatch publish failed");
    }
}

/// Sum every topic's cached value. `None` if the list is empty or any topic
/// isn't cached yet.
fn sum_cached(topics: &[String], cache: &InputCache) -> Option<f64> {
    if topics.is_empty() {
        return None;
    }
    let mut total = 0.0;
    for topic in topics {
        total += cache.get(topic)?.0;
    }
    Some(total)
}

#[cfg(test)]
#[path = "actual_power_test.rs"]
mod tests;
