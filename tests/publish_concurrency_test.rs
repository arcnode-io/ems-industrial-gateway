//! e2e: many concurrent publishes all go out. High-risk: paho's outbound
//! buffer defaults to 100 messages even while connected, and the gateway
//! has hundreds of device pollers and synthetic tasks each with a publish in
//! flight, so past 100 at once publishes were refused ("Max buffered
//! messages") and readings silently dropped.

mod fixtures;

use anyhow::Result;
use ems_industrial_gateway::mqtt::publisher;
use fixtures::containers::{start_hivemq, unique_network};
use futures::future::join_all;

/// More concurrent publishes than paho's default buffer holds.
const CONCURRENT: usize = 1_000;

#[tokio::test]
async fn a_burst_of_concurrent_publishes_all_go_out() -> Result<()> {
    // Arrange
    let network = unique_network();
    let hivemq = start_hivemq(&network).await?;
    let url = format!("tcp://localhost:{}", hivemq.get_host_port_ipv4(1883).await?);
    let client = publisher::connect(&url, "burst-test", "arcnode_gateway", "test").await?;
    // Act — every publish in flight at once, as pollers do
    let results = join_all((0..CONCURRENT).map(|i| {
        let client = client.clone();
        async move {
            publisher::publish_measurement(
                &client,
                &format!("sites/s/devices/d{i}/measurements/m/watts"),
                1.0,
            )
            .await
        }
    }))
    .await;
    // Assert
    let failed = results.iter().filter(|r| r.is_err()).count();
    assert_eq!(failed, 0, "{failed} of {CONCURRENT} publishes refused");
    Ok(())
}
