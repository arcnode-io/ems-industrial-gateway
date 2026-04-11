#![allow(missing_docs, clippy::missing_docs_in_private_items)]

mod config;
use ems_industrial_gateway::app;

/// Application entry point that loads configuration, sets up logging, and runs the app.
///
/// Loads configuration from cfg.yml, initializes the logger with the configured level,
/// logs the configuration, and calls the main app function with hardcoded values.
///
/// # Panics
/// Panics if configuration cannot be loaded from cfg.yml file
fn main() {
    let cfg = config::load_config().unwrap();
    config::setup_logger(&cfg);
    tracing::info!("Running with: {:?}", cfg);

    app(1, 2);
}
