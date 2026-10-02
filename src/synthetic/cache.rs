//! Shared cache of the latest reading per MQTT topic, as published: a
//! number, a boolean or an enum label.
//!
//! Populated by the MQTT subscriber on incoming samples. Read by
//! every synthetic task on its tick. DashMap chosen so reads + writes are
//! lock-free in practice (sharded RwLocks under the hood).

use dashmap::DashMap;
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;

/// One cache entry: the latest reading + when it landed.
pub type CacheEntry = (Value, Instant);

/// A cached reading as a number: numbers as-is, booleans as 1/0 (e.g. an
/// event flag); a label has none.
#[must_use]
pub fn as_number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_bool().map(|b| if b { 1.0 } else { 0.0 }))
}

/// Topic → (latest value, received-at).
pub type InputCache = Arc<DashMap<String, CacheEntry>>;

/// Build a fresh, empty cache. Caller passes the Arc to both the MQTT
/// subscriber (writer) and every synthetic task (reader).
#[must_use]
pub fn new_input_cache() -> InputCache {
    Arc::new(DashMap::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_input_cache_starts_empty() {
        // Arrange / Act
        let cache = new_input_cache();
        // Assert
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn cache_round_trip_returns_inserted_value() {
        // Arrange
        let cache = new_input_cache();
        let topic = "sites/acme/devices/oe_1/measurements/import_limit/watts".to_string();
        // Act
        cache.insert(topic.clone(), (serde_json::json!(42.5), Instant::now()));
        // Assert
        let entry = cache.get(&topic).expect("topic should be cached");
        assert_eq!(as_number(&entry.0), Some(42.5));
    }
}
