//! Everything related to the `der_dispatch` virtual device: the site-total
//! `actual_active_power` publisher, and site→module distribution (splitting
//! der_dispatch's `target_active_power` across whatever `bess_module`
//! devices exist).

mod actual_power;
mod site_distribution;

pub use actual_power::{DerDispatchTaskConfig, spawn};
pub use site_distribution::{SiteDistributionConfig, spawn as spawn_site_distribution};
