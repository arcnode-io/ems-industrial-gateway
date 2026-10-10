//! One async task per synthetic channel: tick → read cached inputs → apply
//! operation → publish FloatSample.
//!
//! Hold semantic (handoff Q5b): does NOT publish unless every declared input
//! topic has a cached sample, younger than its stale limit if it has one. Consumers watching the output topic
//! see no traffic during cold start / outage; quality is recoverable from
//! the input channels' own status measurements per ADR §5.

use crate::synthetic::cache::{CacheEntry, InputCache, as_number};
use crate::synthetic::operation::{self, Operation};
use anyhow::Result;
use chrono::Utc;
use paho_mqtt::{AsyncClient, Message};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// MQTT QoS for synthetic publishes — matches ADR-002 §11 measurement family.
const QOS_MEASUREMENT: i32 = 0;

/// What a synthetic task computes each tick — the two wire-shape modes
/// (`inputs[]` vs `pairs[]`) are mutually exclusive, so this is a real
/// either/or, not two optional fields.
pub enum Computation {
    /// Apply `operation` to the cached values at `input_topics`, in order.
    Operation {
        /// Parsed operation (validated at gateway startup, not runtime).
        operation: Operation,
        /// Topics this task reads from the shared cache on each tick.
        input_topics: Vec<String>,
    },
    /// Apply `weighted_mean` to the cached value at each pair's topic,
    /// weighted by that pair's static weight (e.g. a rack's `capacity_kwh`).
    WeightedMean {
        /// `(topic, weight)` pairs this task reads on each tick.
        pairs: Vec<(String, f64)>,
    },
}

/// Everything one synthetic task needs to run forever.
pub struct SyntheticTaskConfig {
    /// Canonical output topic (already site_id-substituted by caller).
    pub output_topic: String,
    /// What this task computes each tick.
    pub computation: Computation,
    /// Tick cadence in Hz; derived from the measurement's poll_rate_hz.
    pub tick_hz: f64,
    /// Input topic → age that holds the publish (see `stale::stale_limits`);
    /// an input not listed never goes stale.
    pub stale_limits: HashMap<String, Duration>,
}

/// Spawn the per-channel synthetic loop. The returned `JoinHandle` exits when
/// `cancel` fires — gateway shutdown cancels the parent token and joins on the
/// returned handle. Without the cancel hook the gateway would never finish
/// `task_handles.join_next().await` because the synthetic loop ticked forever.
pub fn spawn(
    cfg: SyntheticTaskConfig,
    cache: InputCache,
    mqtt: AsyncClient,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let period_ms = hz_to_period_ms(cfg.tick_hz);
        let mut ticker = interval(Duration::from_millis(period_ms));
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    if let Err(err) = tick_once(&cfg, &cache, &mqtt).await {
                        warn!(
                            topic = %cfg.output_topic,
                            error = %err,
                            "synthetic tick error",
                        );
                    }
                }
            }
        }
    })
}

/// One tick: gather cached input values; if any input is missing or stale, hold (no
/// publish); otherwise evaluate + publish.
async fn tick_once(
    cfg: &SyntheticTaskConfig,
    cache: &InputCache,
    mqtt: &AsyncClient,
) -> Result<()> {
    let result = match &cfg.computation {
        Computation::Operation {
            operation,
            input_topics,
        } => {
            let Some(values) = gather_inputs(input_topics, cache, &cfg.stale_limits) else {
                debug!(
                    topic = %cfg.output_topic,
                    "synthetic hold: not all inputs cached yet",
                );
                return Ok(());
            };
            operation.apply(&values)?
        }
        Computation::WeightedMean { pairs } => {
            let Some(resolved) = gather_pairs(pairs, cache, &cfg.stale_limits) else {
                debug!(
                    topic = %cfg.output_topic,
                    "synthetic hold: not all pairs cached yet",
                );
                return Ok(());
            };
            operation::weighted_mean(&resolved)?
        }
    };
    let payload = format!(
        r#"{{"ts":"{ts}","value":{value}}}"#,
        ts = Utc::now().to_rfc3339(),
        value = result,
    );
    let msg = Message::new(&cfg.output_topic, payload.into_bytes(), QOS_MEASUREMENT);
    mqtt.publish(msg).await?;
    Ok(())
}

/// Return Some(values) if EVERY input topic has a fresh cached entry; None if any
/// input is missing (hold semantic per Q5b).
fn gather_inputs(
    input_topics: &[String],
    cache: &InputCache,
    stale_limits: &HashMap<String, Duration>,
) -> Option<Vec<f64>> {
    let mut values = Vec::with_capacity(input_topics.len());
    for topic in input_topics {
        let entry = fresh(cache, topic, stale_limits)?;
        values.push(as_number(&entry.0)?);
    }
    Some(values)
}

/// Return Some((value, weight)) pairs if EVERY pair's topic has a fresh cached
/// entry; None if any is missing (same hold semantic as `gather_inputs`).
fn gather_pairs(
    pairs: &[(String, f64)],
    cache: &InputCache,
    stale_limits: &HashMap<String, Duration>,
) -> Option<Vec<(f64, f64)>> {
    let mut resolved = Vec::with_capacity(pairs.len());
    for (topic, weight) in pairs {
        let entry = fresh(cache, topic, stale_limits)?;
        resolved.push((as_number(&entry.0)?, *weight));
    }
    Some(resolved)
}

/// `topic`'s cached entry, unless it's older than its stale limit.
fn fresh<'a>(
    cache: &'a InputCache,
    topic: &str,
    stale_limits: &HashMap<String, Duration>,
) -> Option<dashmap::mapref::one::Ref<'a, String, CacheEntry>> {
    let entry = cache.get(topic)?;
    let dead = stale_limits
        .get(topic)
        .is_some_and(|l| entry.1.elapsed() > *l);
    (!dead).then_some(entry)
}

/// Convert poll_rate_hz to a tick period in milliseconds; min 1ms so the
/// ticker never panics on zero/sub-ms values (clamped upstream too).
fn hz_to_period_ms(hz: f64) -> u64 {
    let period = (1000.0 / hz).max(1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        period as u64
    }
}

#[cfg(test)]
#[path = "task_test.rs"]
mod tests;
