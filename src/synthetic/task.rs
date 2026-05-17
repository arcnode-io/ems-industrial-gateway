//! One async task per synthetic channel: tick → read cached inputs → apply
//! formula → publish FloatSample.
//!
//! Hold semantic (handoff Q5b): does NOT publish until every declared input
//! topic has at least one cached sample. Consumers watching the output topic
//! see no traffic during cold start / outage; quality is recoverable from
//! the input channels' own status measurements per ADR §5.

use crate::synthetic::cache::InputCache;
use crate::synthetic::formula::Formula;
use anyhow::Result;
use chrono::Utc;
use paho_mqtt::{AsyncClient, Message};
use std::time::Duration;
use tokio::time::interval;
use tracing::{debug, warn};

/// MQTT QoS for synthetic publishes — matches ADR-002 §11 measurement family.
const QOS_MEASUREMENT: i32 = 0;

/// Everything one synthetic task needs to run forever.
pub struct SyntheticTaskConfig {
    /// Canonical output topic (already site_id-substituted by caller).
    pub output_topic: String,
    /// Topics this task reads from the shared cache on each tick.
    pub input_topics: Vec<String>,
    /// Parsed formula (validated at gateway startup, not runtime).
    pub formula: Formula,
    /// Tick cadence in Hz; derived from the measurement's poll_rate_hz.
    pub tick_hz: f64,
}

/// Spawn the per-channel synthetic loop. The returned `JoinHandle` lives for
/// the gateway's lifetime; caller is expected to keep it.
pub fn spawn(
    cfg: SyntheticTaskConfig,
    cache: InputCache,
    mqtt: AsyncClient,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let period_ms = hz_to_period_ms(cfg.tick_hz);
        let mut ticker = interval(Duration::from_millis(period_ms));
        loop {
            ticker.tick().await;
            if let Err(err) = tick_once(&cfg, &cache, &mqtt).await {
                warn!(
                    topic = %cfg.output_topic,
                    error = %err,
                    "synthetic tick error",
                );
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
    let Some(values) = gather_inputs(&cfg.input_topics, cache) else {
        debug!(
            topic = %cfg.output_topic,
            "synthetic hold: not all inputs cached yet",
        );
        return Ok(());
    };
    let result = cfg.formula.apply(&values)?;
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
    fn hz_to_period_clamps_to_minimum_1ms() {
        assert_eq!(hz_to_period_ms(2000.0), 1);
        assert_eq!(hz_to_period_ms(1.0), 1000);
        assert_eq!(hz_to_period_ms(0.0033), 303_030);
    }
}
