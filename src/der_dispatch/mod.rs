//! Everything related to the `der_dispatch` virtual device: the site-total
//! `actual_active_power` publisher, and site→module distribution (splitting
//! der_dispatch's `target_active_power` across whatever `bess_module`
//! devices exist).

mod actual_power;
mod event_memory;
mod module_bounds;
mod module_command;
mod posture;
mod site_distribution;

pub use actual_power::{DerDispatchTaskConfig, spawn};
pub use event_memory::{SharedEventMemory, new_event_memory};
pub use module_command::Devices;
pub use posture::PostureTopics;
pub use site_distribution::{SiteDistributionConfig, spawn as spawn_site_distribution};
