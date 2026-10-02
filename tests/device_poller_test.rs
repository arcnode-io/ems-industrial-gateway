//! e2e: one poller per device. High-risk: a task per reading means a
//! device with N readings gets N concurrent requests. Across ~100 GPU nodes
//! that's ~350 connections to one service, which exhausted the mock's file
//! descriptors; a real BMC allows a handful of sessions and refuses the
//! rest.

mod fixtures;

use anyhow::Result;
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;

const SITE_ID: &str = "local_site";
/// Readings on the one device, each its own URI.
const READINGS: usize = 8;

/// Requests in flight now, and the most ever in flight at once.
#[derive(Default)]
struct Concurrency {
    now: AtomicUsize,
    peak: AtomicUsize,
    served: AtomicUsize,
}

/// A slow BMC: each request takes 100 ms, so overlapping requests show.
async fn spawn_bmc(stats: Arc<Concurrency>) -> Result<u16> {
    let app = Router::new()
        .route("/redfish/v1/Sensors/{n}", get(slow_reading))
        .with_state(stats);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(async move { axum::serve(listener, app).await });
    Ok(port)
}

async fn slow_reading(State(stats): State<Arc<Concurrency>>) -> Json<Value> {
    let now = stats.now.fetch_add(1, Ordering::SeqCst) + 1;
    stats.peak.fetch_max(now, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(100)).await;
    stats.now.fetch_sub(1, Ordering::SeqCst);
    stats.served.fetch_add(1, Ordering::SeqCst);
    Json(json!({ "Reading": 42.0 }))
}

/// A Redfish reading at `uri` on the local BMC, polled at 1 Hz.
fn reading(port: u16, uri: &str, pointer: &str) -> Value {
    json!({
        "unit": "celsius", "poll_rate_hz": 1.0, "protocol": "redfish",
        "host": "127.0.0.1", "port": port, "uri": uri, "json_pointer": pointer,
    })
}

#[tokio::test]
async fn a_device_is_polled_one_request_at_a_time() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    // Arrange — one device with eight readings at 1 Hz, behind a slow BMC
    let stats = Arc::new(Concurrency::default());
    let port = spawn_bmc(stats.clone()).await?;
    let readings = (1..=READINGS)
        .map(|n| {
            (
                format!("sensor_{n}"),
                reading(port, &format!("/Sensors/{n}"), "/Reading"),
            )
        })
        .collect();
    // Act
    fixtures::gateway::poll_for(SITE_ID, readings, 4).await?;
    // Assert — every reading was polled, never two at once
    let served = stats.served.load(Ordering::SeqCst);
    assert!(served >= READINGS * 2, "only {served} requests served");
    assert_eq!(
        stats.peak.load(Ordering::SeqCst),
        1,
        "concurrent requests to one device"
    );
    Ok(())
}

/// Requests per resource.
type Hits = Arc<Mutex<HashMap<String, usize>>>;

/// A BMC whose two resources each serve two values, counting requests.
async fn spawn_counting_bmc(hits: Hits) -> Result<u16> {
    let app = Router::new()
        .route(
            "/redfish/v1/Gpu/{n}",
            get(
                |State(hits): State<Hits>, Path(n): Path<String>| async move {
                    *hits.lock().unwrap().entry(n).or_default() += 1;
                    Json(json!({ "PowerWatts": 1000.0, "Clock": 1980.0 }))
                },
            ),
        )
        .with_state(hits);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(async move { axum::serve(listener, app).await });
    Ok(port)
}

#[tokio::test]
async fn readings_from_one_resource_share_one_fetch() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    // Arrange — four readings over two resources (power and clock of each GPU)
    let hits: Hits = Arc::default();
    let port = spawn_counting_bmc(hits.clone()).await?;
    let readings = [1, 2]
        .iter()
        .flat_map(|g| {
            let uri = format!("/Gpu/{g}");
            [
                (format!("gpu_{g}_power"), reading(port, &uri, "/PowerWatts")),
                (format!("gpu_{g}_clock"), reading(port, &uri, "/Clock")),
            ]
        })
        .collect();
    // Act — about four 1 Hz ticks
    fixtures::gateway::poll_for(SITE_ID, readings, 4).await?;
    // Assert — each resource fetched about once a tick, not once per reading
    for (gpu, count) in hits.lock().unwrap().iter() {
        assert!(
            (3..=5).contains(count),
            "GPU {gpu} fetched {count} times in ~4 ticks"
        );
    }
    assert_eq!(hits.lock().unwrap().len(), 2, "both resources polled");
    Ok(())
}

#[tokio::test]
async fn an_unreachable_device_does_not_hold_up_shutdown() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    // Arrange — four readings on a port nothing listens on: every read
    // retries with backoff for ~15 s
    let closed = TcpListener::bind("127.0.0.1:0").await?.local_addr()?.port();
    let readings = (1..=4)
        .map(|n| {
            (
                format!("sensor_{n}"),
                reading(closed, &format!("/Sensors/{n}"), "/Reading"),
            )
        })
        .collect();
    // Act — poll 2 s, then stop
    let shutdown = fixtures::gateway::poll_for(SITE_ID, readings, 2).await?;
    // Assert — stopped at once, not after the retries ran out
    assert!(
        shutdown < Duration::from_secs(2),
        "gateway took {shutdown:?} to stop"
    );
    Ok(())
}
