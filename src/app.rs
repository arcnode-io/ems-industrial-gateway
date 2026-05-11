//! Boot orchestration. Tier 1: one-shot fetch → modbus read → MQTT publish → exit.

use crate::config::Config;
use tracing::info;

/// Run the gateway one-shot. Filled in by subsequent tasks.
pub async fn run(cfg: Config) -> anyhow::Result<()> {
    info!(
        device_api_url = %cfg.device_api_url,
        broker_url = %cfg.broker_url,
        site_id = %cfg.site_id,
        "gateway starting",
    );
    anyhow::bail!("not implemented yet")
}
