//! Whether the operator lets storage answer the envelope
//! (`der_dispatch/storage_authorized`, published by ems-der-control-api).
//! The envelope is mandatory either way; this only picks the resource. When
//! withheld, the battery may not discharge for it and compute shedding
//! covers the gap instead. Charging stays allowed.

use crate::synthetic::InputCache;

/// `der_dispatch`'s storage_authorized measurement for `site_id`.
pub fn topic(site_id: &str) -> String {
    format!("sites/{site_id}/devices/der_dispatch/measurements/storage_authorized/none")
}

/// True only once the operator has published `false`.
pub fn withheld(cache: &InputCache, site_id: &str) -> bool {
    cache
        .get(&topic(site_id))
        .is_some_and(|e| e.0 == serde_json::Value::Bool(false))
}

#[cfg(test)]
#[path = "storage_authorized_test.rs"]
mod tests;
