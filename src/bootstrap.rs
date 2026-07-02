//! On-prem self-configuration from the customer's CFN stack.
//!
//! Cloud customers download the gateway tarball from the delivery portal and
//! run it on their plant network with AWS env creds:
//!
//! ```sh
//! docker run -e AWS_REGION=... -e AWS_ACCESS_KEY_ID=... \
//!   -e AWS_SECRET_ACCESS_KEY=... -e ARCNODE_STACK_NAME=<their stack> \
//!   ems-industrial-gateway
//! ```
//!
//! When `ARCNODE_STACK_NAME` is set, the gateway reads the stack's outputs
//! (`SiteId`, `BrokerWsUrl`, `DeviceApiUrl`, `GatewaySecretName` — the
//! bootstrap contract rendered by platform-api's CfnService) and the broker
//! password from Secrets Manager, then boots. No hand-edited cfg, no setup
//! script. Absent the env var, the normal cfg.defaults.yml + CFG_CUSTOMER_PATH
//! path applies (compose-mounted deployments).

use crate::config::{Config, load_config_from};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::env;
use std::path::Path;
use tracing::info;

/// Env var carrying the customer's CFN stack name — presence enables bootstrap.
const STACK_ENV: &str = "ARCNODE_STACK_NAME";

/// If `ARCNODE_STACK_NAME` is set, build the Config from the stack's outputs.
/// Returns `None` when unset so main falls through to the file loader.
/// Fail-loud on any missing output — a partial contract is a platform bug,
/// not something to limp past.
pub async fn config_from_stack() -> Result<Option<Config>> {
    let Ok(stack_name) = env::var(STACK_ENV) else {
        return Ok(None);
    };
    let aws = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let cfn = aws_sdk_cloudformation::Client::new(&aws);
    let stacks = cfn
        .describe_stacks()
        .stack_name(&stack_name)
        .send()
        .await
        .with_context(|| format!("describe-stacks {stack_name} — check AWS creds/region"))?;
    let outputs: BTreeMap<String, String> = stacks
        .stacks()
        .first()
        .with_context(|| format!("stack {stack_name} not found"))?
        .outputs()
        .iter()
        .filter_map(|o| Some((o.output_key()?.to_string(), o.output_value()?.to_string())))
        .collect();
    // Base block carries the fields the stack doesn't decide (mqtt_username,
    // log_level); the outputs override the deployment-specific ones.
    let base = load_config_from(Path::new("cfg.defaults.yml"), None, "beta")
        .context("cfg.defaults.yml beta block")?;
    let cfg = config_from_outputs(&outputs, base)?;
    info!(stack = %stack_name, site_id = %cfg.site_id, "self-configured from stack outputs");
    Ok(Some(cfg))
}

/// Pure core — map the bootstrap-contract outputs onto a base Config.
fn config_from_outputs(outputs: &BTreeMap<String, String>, base: Config) -> Result<Config> {
    let get = |key: &str| {
        outputs
            .get(key)
            .cloned()
            .with_context(|| format!("stack output {key} missing — platform contract broken"))
    };
    Ok(Config {
        device_api_url: get("DeviceApiUrl")?,
        broker_url: get("BrokerWsUrl")?,
        site_id: get("SiteId")?,
        ..base
    })
}

/// Fetch the broker password from Secrets Manager via the stack's
/// `GatewaySecretName` output. Called by app::run only when
/// `MQTT_GATEWAY_PASSWORD` isn't in the env (the on-prem path).
pub async fn fetch_gateway_password() -> Result<String> {
    let stack_name =
        env::var(STACK_ENV).context("MQTT_GATEWAY_PASSWORD unset and ARCNODE_STACK_NAME unset")?;
    let aws = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let cfn = aws_sdk_cloudformation::Client::new(&aws);
    let stacks = cfn
        .describe_stacks()
        .stack_name(&stack_name)
        .send()
        .await
        .with_context(|| format!("describe-stacks {stack_name}"))?;
    let secret_name = stacks
        .stacks()
        .first()
        .and_then(|s| {
            s.outputs()
                .iter()
                .find(|o| o.output_key() == Some("GatewaySecretName"))
        })
        .and_then(|o| o.output_value())
        .context("stack output GatewaySecretName missing")?
        .to_string();
    let sm = aws_sdk_secretsmanager::Client::new(&aws);
    let secret = sm
        .get_secret_value()
        .secret_id(&secret_name)
        .send()
        .await
        .with_context(|| format!("get-secret-value {secret_name}"))?;
    secret
        .secret_string()
        .map(str::to_string)
        .context("gateway secret has no string value")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Config {
        Config {
            device_api_url: "http://device-api:3000".into(),
            broker_url: "tcp://hivemq:1883".into(),
            mqtt_username: "arcnode_gateway".into(),
            site_id: "default".into(),
            log_level: "info".into(),
            gateway_credentials: None,
        }
    }

    fn contract() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("SiteId".into(), "brookside".into()),
            ("BrokerWsUrl".into(), "ws://1.2.3.4/mqtt".into()),
            ("DeviceApiUrl".into(), "http://1.2.3.4/api".into()),
            (
                "GatewaySecretName".into(),
                "arcnode-ems-x/mqtt-gateway-password".into(),
            ),
        ])
    }

    #[test]
    fn maps_contract_outputs_onto_base() {
        // Arrange
        let outputs = contract();

        // Act
        let cfg = config_from_outputs(&outputs, base()).unwrap();

        // Assert — stack decides the endpoints + site; defaults keep the rest
        assert_eq!(cfg.broker_url, "ws://1.2.3.4/mqtt");
        assert_eq!(cfg.device_api_url, "http://1.2.3.4/api");
        assert_eq!(cfg.site_id, "brookside");
        assert_eq!(cfg.mqtt_username, "arcnode_gateway");
    }

    #[test]
    fn missing_output_fails_loud() {
        // Arrange
        let mut outputs = contract();
        outputs.remove("BrokerWsUrl");

        // Act
        let err = config_from_outputs(&outputs, base()).unwrap_err();

        // Assert
        assert!(err.to_string().contains("BrokerWsUrl"));
    }
}
