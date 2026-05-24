//! cfg loader. ENV picks stage in cfg.defaults.yml; CFG_CUSTOMER_PATH (if set)
//! deep-merges over it before deserialization.

use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Paths to the gateway's mTLS material — mounted as secrets at deploy time.
/// All three paths are config (cfg.yml); the file CONTENTS are secrets.
/// `None` on the parent `Config` means TLS isn't provisioned and any device
/// requiring `tls_mutual` trust will fail-fast at task spawn.
#[derive(Debug, Deserialize, Clone)]
pub struct GatewayCredentials {
    /// CA trust bundle the gateway validates device certs against.
    pub ca_bundle_path: PathBuf,
    /// Gateway's own CA-issued cert (presented in mTLS handshake).
    pub cert_path: PathBuf,
    /// Gateway's private key matching `cert_path`.
    pub key_path: PathBuf,
}

/// Gateway runtime config — one stage block from cfg.defaults.yml after
/// any cfg.customer.yml merge.
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// HTTP base URL for device-api (e.g. `http://device-api:3000`).
    pub device_api_url: String,
    /// MQTT broker URL (e.g. `tcp://hivemq:1883`).
    pub broker_url: String,
    /// Site identifier used in MQTT topic paths (`sites/{site_id}/...`).
    pub site_id: String,
    /// Log verbosity: `error | warn | info | debug`.
    pub log_level: String,
    /// mTLS material (CA bundle + gateway cert/key). `None` for pre-PKI
    /// deployments; set in cfg.customer.yml when TLS gets provisioned.
    #[serde(default)]
    pub gateway_credentials: Option<GatewayCredentials>,
}

/// Production loader — reads `ENV` + `CFG_CUSTOMER_PATH` from process env.
pub fn load_config() -> anyhow::Result<Config> {
    load_config_from(
        Path::new("cfg.defaults.yml"),
        env::var("CFG_CUSTOMER_PATH").ok().as_deref(),
        env::var("ENV")
            .unwrap_or_else(|_| "local".to_string())
            .as_str(),
    )
}

/// Pure fn for tests. Reads defaults; if customer_path exists, deep-merges
/// over the matching stage block before deserialization.
pub fn load_config_from(
    defaults_path: &Path,
    customer_path: Option<&str>,
    env_name: &str,
) -> anyhow::Result<Config> {
    let defaults_raw = fs::read_to_string(defaults_path)?;
    let mut all: Value = serde_yaml::from_str(&defaults_raw)?;
    if let Some(path) = customer_path {
        let customer_path = Path::new(path);
        if customer_path.exists() {
            let customer_raw = fs::read_to_string(customer_path)?;
            let customer: Value = serde_yaml::from_str(&customer_raw)?;
            merge_into_stage(&mut all, env_name, customer);
        }
    }
    let block = all
        .get(env_name)
        .ok_or_else(|| anyhow::anyhow!("cfg.defaults.yml missing block: {env_name}"))?;
    let cfg: Config = serde_yaml::from_value(block.clone())?;
    Ok(cfg)
}

/// Deep-merge `customer` onto `all[env_name]`. Nested mappings recurse; scalars + sequences overwrite.
fn merge_into_stage(all: &mut Value, env_name: &str, customer: Value) {
    let Value::Mapping(top) = all else { return };
    let Some(Value::Mapping(stage)) = top.get_mut(env_name) else {
        return;
    };
    if let Value::Mapping(over) = customer {
        deep_merge(stage, over);
    }
}

/// In-place deep-merge of `over` onto `base` — nested mappings recurse, scalars + sequences overwrite.
fn deep_merge(base: &mut Mapping, over: Mapping) {
    for (key, val) in over {
        match (base.get_mut(&key), val) {
            (Some(Value::Mapping(b)), Value::Mapping(o)) => deep_merge(b, o),
            (_, v) => {
                base.insert(key, v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_yaml(s: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(s.as_bytes()).unwrap();
        f
    }

    const DEFAULTS: &str = r#"
local:
  device_api_url: http://localhost:3000
  broker_url: tcp://localhost:1883
  site_id: site_001
  log_level: info
beta:
  device_api_url: http://device-api:3000
  broker_url: tcp://hivemq:1883
  site_id: arcnode_beta
  log_level: info
"#;

    #[test]
    fn defaults_only_picks_stage() {
        let defaults = write_yaml(DEFAULTS);
        let cfg = load_config_from(defaults.path(), None, "local").unwrap();
        assert_eq!(cfg.site_id, "site_001");
        assert_eq!(cfg.broker_url, "tcp://localhost:1883");
    }

    #[test]
    fn customer_yml_overrides_site_id_only() {
        let defaults = write_yaml(DEFAULTS);
        let customer = write_yaml("site_id: nevada_facility_2\n");
        let cfg = load_config_from(defaults.path(), customer.path().to_str(), "beta").unwrap();
        // customer wins for site_id
        assert_eq!(cfg.site_id, "nevada_facility_2");
        // baked defaults survive for untouched keys
        assert_eq!(cfg.broker_url, "tcp://hivemq:1883");
    }

    #[test]
    fn customer_yml_can_set_gateway_credentials_for_tls() {
        // Arrange — customer cfg supplies CA bundle + gateway cert/key paths.
        // Paths are config; the files they point at are mounted secrets.
        let defaults = write_yaml(DEFAULTS);
        let customer = write_yaml(
            "gateway_credentials:\n  ca_bundle_path: /etc/secrets/ca-bundle.crt\n  cert_path: /etc/secrets/gateway.crt\n  key_path: /etc/secrets/gateway.key\n",
        );
        let cfg = load_config_from(defaults.path(), customer.path().to_str(), "beta").unwrap();
        // Assert — all three paths land in the parsed config
        let creds = cfg.gateway_credentials.expect("gateway_credentials parsed");
        assert_eq!(
            creds.ca_bundle_path,
            PathBuf::from("/etc/secrets/ca-bundle.crt")
        );
        assert_eq!(creds.cert_path, PathBuf::from("/etc/secrets/gateway.crt"));
        assert_eq!(creds.key_path, PathBuf::from("/etc/secrets/gateway.key"));
    }

    #[test]
    fn defaults_without_gateway_credentials_yields_none() {
        // Arrange — DEFAULTS has no gateway_credentials block.
        let defaults = write_yaml(DEFAULTS);
        // Act
        let cfg = load_config_from(defaults.path(), None, "local").unwrap();
        // Assert — Option<> is None when block is absent (back-compat).
        assert!(cfg.gateway_credentials.is_none());
    }

    #[test]
    fn missing_customer_path_falls_back_to_defaults() {
        let defaults = write_yaml(DEFAULTS);
        let cfg = load_config_from(
            defaults.path(),
            Some("/nonexistent/path/cfg.customer.yml"),
            "beta",
        )
        .unwrap();
        assert_eq!(cfg.site_id, "arcnode_beta");
    }
}
