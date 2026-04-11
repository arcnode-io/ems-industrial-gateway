use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::error::Error;
use std::{env, fs};
use validator::Validate;

#[derive(Debug, PartialEq, Deserialize, Serialize, Clone)]
#[allow(clippy::upper_case_acronyms)]
pub enum LogLevel {
    ERROR,
    WARN,
    INFO,
    DEBUG,
}

#[derive(Validate, Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct Config {
    pub log_level: LogLevel,
}

#[derive(Debug, Deserialize, Serialize)]
struct ConfigMap {
    local: Config,
    beta: Config,
}

/// Loads configuration from cfg.yml file based on environment variable.
///
/// Reads cfg.yml, applies YAML merge operations, and returns configuration
/// for the current environment (ENV variable or "local" as default).
///
/// # Returns
/// Configuration struct for the current environment
///
/// # Errors
/// Returns error if cfg.yml cannot be read or parsed
///
/// # Panics
/// Panics if YAML parsing or merge operations fail
///
/// # Example
/// ```
/// let config = load_config().unwrap();
/// assert_eq!(config.log_level, Level::INFO);
/// ```
pub fn load_config() -> Result<Config, Box<dyn Error>> {
    let contents = fs::read_to_string("cfg.yml").unwrap();
    let mut config_map: Value = serde_yaml::from_str(&contents).unwrap();
    config_map.apply_merge().unwrap();
    let config_map: ConfigMap = serde_yaml::from_value(config_map).unwrap();

    let environment = env::var("ENV").unwrap_or_else(|_| "local".to_string());
    let config = match environment.as_str() {
        "beta" => config_map.beta,
        _ => config_map.local,
    };

    Ok(config)
}

/// Sets up the global tracing logger with the configured log level.
///
/// Creates a formatted tracing subscriber with the log level from config
/// and sets it as the global default subscriber for the application.
///
/// # Arguments
/// * `config` - Configuration containing the desired log level
///
/// # Panics
/// Panics if setting the global default subscriber fails
///
/// # Example
/// ```
/// let config = Config { log_level: Level::INFO };
/// setup_logger(&config);
/// ```
pub fn setup_logger(config: &Config) {
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(match config.log_level {
            LogLevel::ERROR => tracing::Level::ERROR,
            LogLevel::WARN => tracing::Level::WARN,
            LogLevel::INFO => tracing::Level::INFO,
            LogLevel::DEBUG => tracing::Level::DEBUG,
        })
        .with_target(false)
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set global default subscriber");
}
