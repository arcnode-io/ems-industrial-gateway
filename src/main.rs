//! Binary entry: load cfg, init tracing, run the gateway until SIGINT/SIGTERM.

use ems_industrial_gateway::{app, bootstrap, config::load_config};
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing::info;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    // On-prem cloud-customer path: ARCNODE_STACK_NAME + AWS env creds →
    // self-configure from the stack's outputs. Otherwise the file loader.
    let cfg = match bootstrap::config_from_stack().await? {
        Some(cfg) => cfg,
        None => load_config()?,
    };
    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(match cfg.log_level.as_str() {
            "error" => tracing::Level::ERROR,
            "warn" => tracing::Level::WARN,
            "debug" => tracing::Level::DEBUG,
            _ => tracing::Level::INFO,
        })
        .init();

    let cancel = CancellationToken::new();
    let signal_cancel = cancel.clone();
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        signal_cancel.cancel();
    });

    app::run(cfg, cancel).await
}

/// Wait for either SIGINT (Ctrl-C) or SIGTERM (orchestrator stop). Whichever
/// arrives first ends the wait. Unix-only path covers SIGTERM; Ctrl-C works
/// on all platforms.
async fn wait_for_shutdown_signal() {
    let ctrl_c = async {
        let _ = signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) = signal::unix::signal(signal::unix::SignalKind::terminate()) {
            sig.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("SIGINT received"),
        _ = terminate => info!("SIGTERM received"),
    }
}
