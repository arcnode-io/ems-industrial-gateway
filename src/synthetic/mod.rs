//! Gateway-side synthetic measurement engine.
//!
//! Per ADR (handoff §B): a "synthetic" channel publishes a value computed
//! from cached MQTT inputs via a named pure function. The gateway subscribes
//! to declared input topics, caches the latest float value per topic, and
//! periodically (per the measurement's `poll_rate_hz`) evaluates the formula
//! over the cached values and publishes a `FloatSample` to the channel's
//! canonical MQTT address.
//!
//! Cold-start semantic (handoff Q5b): a synthetic task does NOT publish until
//! every input topic in its declared `inputs[]` has received at least one
//! sample. This matches ADR §5 strict `{ts, value}` payloads — quality is
//! recoverable from the underlying input channels' status measurements.

pub mod cache;
pub mod formula;
pub mod task;

pub use cache::{InputCache, new_input_cache};
pub use formula::Formula;
pub use task::SyntheticTaskConfig;
