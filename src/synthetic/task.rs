//! One async task per synthetic channel: tick → read cached inputs → apply
//! operation → publish FloatSample.
//!
//! Hold semantic (handoff Q5b): does NOT publish until every declared input
//! topic has at least one cached sample. Consumers watching the output topic
//! see no traffic during cold start / outage; quality is recoverable from
//! the input channels' own status measurements per ADR §5.

use crate::synthetic::cache::InputCache;
use crate::synthetic::operation::{self, Operation};
use anyhow::Result;
use chrono::Utc;
use paho_mqtt::{AsyncClient, Message};
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

/// One tick: gather cached input values; if any input is missing, hold (no
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
            let Some(values) = gather_inputs(input_topics, cache) else {
                debug!(
                    topic = %cfg.output_topic,
                    "synthetic hold: not all inputs cached yet",
                );
                return Ok(());
            };
            operation.apply(&values)?
        }
        Computation::WeightedMean { pairs } => {
            let Some(resolved) = gather_pairs(pairs, cache) else {
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

/// Return Some(values) if EVERY input topic has a cached entry; None if any
/// input is missing (hold semantic per Q5b).
fn gather_inputs(input_topics: &[String], cache: &InputCache) -> Option<Vec<f64>> {
    let mut values = Vec::with_capacity(input_topics.len());
    for topic in input_topics {
        let entry = cache.get(topic)?;
        values.push(entry.0);
    }
    Some(values)
}

/// Return Some((value, weight)) pairs if EVERY pair's topic has a cached
/// entry; None if any is missing (same hold semantic as `gather_inputs`).
fn gather_pairs(pairs: &[(String, f64)], cache: &InputCache) -> Option<Vec<(f64, f64)>> {
    let mut resolved = Vec::with_capacity(pairs.len());
    for (topic, weight) in pairs {
        let entry = cache.get(topic)?;
        resolved.push((entry.0, *weight));
    }
    Some(resolved)
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
mod tests {
    use super::*;
    use crate::synthetic::cache::new_input_cache;
    use std::time::Instant;

    #[test]
    fn gather_inputs_holds_when_any_input_missing() {
        // Arrange — one of two topics not yet cached
        let cache = new_input_cache();
        cache.insert("a".into(), (10.0, Instant::now()));
        // Act
        let result = gather_inputs(&["a".into(), "b".into()], &cache);
        // Assert
        assert!(result.is_none(), "hold when any input missing");
    }

    #[test]
    fn gather_inputs_returns_values_when_all_cached() {
        // Arrange — both inputs cached
        let cache = new_input_cache();
        cache.insert("a".into(), (10.0, Instant::now()));
        cache.insert("b".into(), (3.0, Instant::now()));
        // Act
        let values = gather_inputs(&["a".into(), "b".into()], &cache).unwrap();
        // Assert
        assert_eq!(values, vec![10.0, 3.0]);
    }

    #[test]
    fn gather_pairs_holds_when_any_pair_missing() {
        // Arrange — one of two topics not yet cached
        let cache = new_input_cache();
        cache.insert("a".into(), (50.0, Instant::now()));
        // Act
        let result = gather_pairs(&[("a".into(), 2.0), ("b".into(), 1.0)], &cache);
        // Assert
        assert!(result.is_none(), "hold when any pair missing");
    }

    #[test]
    fn gather_pairs_returns_value_weight_pairs_when_all_cached() {
        // Arrange
        let cache = new_input_cache();
        cache.insert("a".into(), (50.0, Instant::now()));
        cache.insert("b".into(), (80.0, Instant::now()));
        // Act
        let pairs = gather_pairs(&[("a".into(), 2.0), ("b".into(), 1.0)], &cache).unwrap();
        // Assert — weight carried through unchanged, value from the cache
        assert_eq!(pairs, vec![(50.0, 2.0), (80.0, 1.0)]);
    }

    #[test]
    fn hz_to_period_clamps_to_minimum_1ms() {
        assert_eq!(hz_to_period_ms(2000.0), 1);
        assert_eq!(hz_to_period_ms(1.0), 1000);
        assert_eq!(hz_to_period_ms(0.0033), 303_030);
    }
}
