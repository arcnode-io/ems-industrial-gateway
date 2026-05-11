use ems_industrial_gateway::{app, config::load_config};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let cfg = load_config()?;
    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(match cfg.log_level.as_str() {
            "error" => tracing::Level::ERROR,
            "warn" => tracing::Level::WARN,
            "debug" => tracing::Level::DEBUG,
            _ => tracing::Level::INFO,
        })
        .init();
    app::run(cfg).await
}
