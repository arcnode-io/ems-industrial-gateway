//! Wait for a freshly-spawned gateway to be subscribed before a test
//! publishes anything it depends on.
//!
//! Test seed readings go out as non-retained QoS 0, so any published before
//! the gateway subscribes are silently dropped by the broker — and gateway
//! startup (MQTT connect + spec fetch) takes anywhere from ~0.3s to 2s+
//! under load. A fixed sleep can't cover that; this waits on a real signal.

use anyhow::{Context, Result};
use futures::stream::StreamExt;
use paho_mqtt::{AsyncClient, ConnectOptionsBuilder, CreateOptionsBuilder, Message};
use std::time::Duration;
use tokio::time::timeout;

/// Upper bound on gateway startup before the test gives up.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Block until the gateway publishes `der_dispatch/actual_active_power`,
/// which it only does once it's subscribed (inputs and commands share one
/// subscribe call), has cached every module's `active_power`, and its task
/// set is running.
///
/// Publishes a retained `active_power` of 0 W for each id in `module_ids` to
/// trigger that — each must be a distribute parent (has a Distribute-bound
/// command) in the test's spec, and all of them must be listed or the sum
/// holds forever.
pub async fn wait_for_gateway_ready(
    broker_url: &str,
    site_id: &str,
    module_ids: &[&str],
) -> Result<()> {
    let mut probe = AsyncClient::new(
        CreateOptionsBuilder::new()
            .server_uri(broker_url)
            .client_id(format!("readiness-probe-{}", std::process::id()))
            .finalize(),
    )?;
    let mut stream = probe.get_stream(16);
    // Reason: the probe publishes measurements, so on an RBAC broker it
    // needs the gateway identity (tests/fixtures/credentials.xml); plain CE
    // ignores credentials.
    probe
        .connect(
            ConnectOptionsBuilder::new()
                .clean_session(true)
                .user_name("arcnode_gateway")
                .password("test")
                .finalize(),
        )
        .await?;
    probe
        .subscribe(
            format!("sites/{site_id}/devices/der_dispatch/measurements/actual_active_power/watts"),
            0,
        )
        .await?;
    for module_id in module_ids {
        probe
            .publish(Message::new_retained(
                format!("sites/{site_id}/devices/{module_id}/measurements/active_power/watts"),
                r#"{"ts":"t","value":0.0}"#,
                0,
            ))
            .await?;
    }
    timeout(READY_TIMEOUT, stream.next())
        .await
        .context("gateway never became ready")?;
    probe.disconnect(None).await?;
    Ok(())
}
