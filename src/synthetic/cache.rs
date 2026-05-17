//! Shared cache of latest float values per MQTT topic.
//!
//! Populated by the MQTT subscriber on incoming FloatSample messages. Read by
//! every synthetic task on its tick. DashMap chosen so reads + writes are
//! lock-free in practice (sharded RwLocks under the hood).

use dashmap::DashMap;
use std::sync::Arc;
use std::time::Instant;

/// One cache entry: the latest float value + when it landed. Instant is for
/// future staleness checks; not used by the formula evaluator today.
pub type CacheEntry = (f64, Instant);

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
        cache.insert(topic.clone(), (42.5, Instant::now()));
        // Assert
        let entry = cache.get(&topic).expect("topic should be cached");
        assert!((entry.0 - 42.5).abs() < f64::EPSILON);
    }
}
