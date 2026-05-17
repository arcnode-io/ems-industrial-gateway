//! cfg.yml deserialize.

use serde::Deserialize;
use std::env;
use std::fs;
use std::path::Path;

/// Gateway runtime configuration.
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// HTTP base URL for the device-api service (e.g., http://device-api:3000).
    pub device_api_url: String,
    /// MQTT broker URL (e.g., tcp://hivemq:1883).
    pub broker_url: String,
    /// Site identifier used in topic paths (sites/{site_id}/...).
    pub site_id: String,
    /// Log verbosity: error | warn | info | debug.
    pub log_level: String,
}

/// Load cfg.yml. Picks `local:` block unless `ENV=beta`. `SITE_ID` env
/// overrides the cfg.yml `site_id` field — operator-supplied site name
/// (slugified by the orchestrator) flows in via UserData → config.env.
pub fn load_config() -> anyhow::Result<Config> {
    let env_name = env::var("ENV").unwrap_or_else(|_| "local".to_string());
    let raw = fs::read_to_string(Path::new("cfg.yml"))?;
    let all: serde_yaml::Value = serde_yaml::from_str(&raw)?;
    let block = all
        .get(&env_name)
        .ok_or_else(|| anyhow::anyhow!("cfg.yml missing block: {env_name}"))?;
    let mut cfg: Config = serde_yaml::from_value(block.clone())?;
    if let Ok(site_id) = env::var("SITE_ID") {
        cfg.site_id = site_id;
    }
    Ok(cfg)
}
