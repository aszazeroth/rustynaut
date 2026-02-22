//! Configuration loading from multiple sources.

use crate::config::error::ConfigError;
use crate::config::paths;
use crate::config::types::*;
use std::fs;
use std::path::Path;

/// Configuration loader supporting layered configuration.
///
/// Loads config from multiple sources in priority order:
/// 1. Default values (hardcoded)
/// 2. Config file (TOML)
/// 3. Environment variables
/// 4. CLI arguments (applied after loading)
pub struct ConfigLoader;

impl ConfigLoader {
    /// Load broker configuration from default locations.
    pub fn load_broker_config(explicit_path: Option<&str>) -> Result<BrokerConfig, ConfigError> {
        let mut config = BrokerConfig::default();

        // Load from file if exists
        if let Some(path) = paths::find_broker_config(explicit_path) {
            let file_config = Self::load_broker_from_file(&path)?;
            config = Self::merge_broker_configs(&config, &file_config);
        }

        // Merge environment variables
        config = Self::merge_broker_from_env(config);

        Ok(config)
    }

    /// Load client configuration from default locations.
    pub fn load_client_config(explicit_path: Option<&str>) -> Result<ClientConfig, ConfigError> {
        let mut config = ClientConfig::default();

        // Load from file if exists
        if let Some(path) = paths::find_client_config(explicit_path) {
            let file_config = Self::load_client_from_file(&path)?;
            config = Self::merge_client_configs(&config, &file_config);
        }

        // Merge environment variables
        config = Self::merge_client_from_env(config);

        Ok(config)
    }

    /// Load broker config from file.
    fn load_broker_from_file(path: &Path) -> Result<BrokerConfig, ConfigError> {
        let content = fs::read_to_string(path).map_err(|e| ConfigError::file_read(path, e))?;

        toml::from_str(&content).map_err(|e| ConfigError::parse(path, e))
    }

    /// Load client config from file.
    fn load_client_from_file(path: &Path) -> Result<ClientConfig, ConfigError> {
        let content = fs::read_to_string(path).map_err(|e| ConfigError::file_read(path, e))?;

        toml::from_str(&content).map_err(|e| ConfigError::parse(path, e))
    }

    /// Merge two broker configs (second takes precedence via serde).
    fn merge_broker_configs(
        defaults: &BrokerConfig,
        override_config: &BrokerConfig,
    ) -> BrokerConfig {
        // Simple override: non-default values from override take precedence
        let mut result = defaults.clone();

        // Server settings
        if override_config.server.bind_address != "0.0.0.0:4242" {
            result.server.bind_address = override_config.server.bind_address.clone();
        }
        if override_config.server.cert_dir != BrokerConfig::default().server.cert_dir {
            result.server.cert_dir = override_config.server.cert_dir.clone();
        }
        if override_config.server.enrollment_enabled
            != BrokerConfig::default().server.enrollment_enabled
        {
            result.server.enrollment_enabled = override_config.server.enrollment_enabled;
        }

        // Limits - always override if set in file
        result.limits = override_config.limits.clone();

        // Logging
        if override_config.logging.level != "info" {
            result.logging.level = override_config.logging.level.clone();
        }
        if override_config.logging.format != "pretty" {
            result.logging.format = override_config.logging.format.clone();
        }

        // Features
        result.features = override_config.features.clone();

        // Timeouts
        result.timeouts = override_config.timeouts.clone();

        result
    }

    /// Merge two client configs (second takes precedence).
    fn merge_client_configs(
        defaults: &ClientConfig,
        override_config: &ClientConfig,
    ) -> ClientConfig {
        let mut result = defaults.clone();

        // Connection settings
        if override_config.connection.broker_address.is_some() {
            result.connection.broker_address = override_config.connection.broker_address.clone();
        }
        if override_config.connection.default_username.is_some() {
            result.connection.default_username =
                override_config.connection.default_username.clone();
        }
        if override_config.connection.default_room.is_some() {
            result.connection.default_room = override_config.connection.default_room.clone();
        }

        // Reconnect settings
        result.connection.reconnect = override_config.connection.reconnect.clone();

        // TLS settings
        result.connection.tls = override_config.connection.tls.clone();

        // Clipboard settings
        result.clipboard = override_config.clipboard.clone();

        // UI settings
        result.ui = override_config.ui.clone();

        // Logging
        if override_config.logging.level != "info" {
            result.logging.level = override_config.logging.level.clone();
        }
        if override_config.logging.format != "pretty" {
            result.logging.format = override_config.logging.format.clone();
        }

        result
    }

    /// Merge broker config with environment variables.
    fn merge_broker_from_env(mut config: BrokerConfig) -> BrokerConfig {
        // Server settings
        if let Ok(val) = std::env::var("RUSTYNAUT_SERVER_BIND_ADDRESS") {
            config.server.bind_address = val;
        }
        if let Ok(val) = std::env::var("RUSTYNAUT_SERVER_CERT_DIR") {
            config.server.cert_dir = std::path::PathBuf::from(val);
        }

        // Limits
        if let Ok(val) = std::env::var("RUSTYNAUT_LIMITS_MAX_CLIENTS") {
            config.limits.max_clients = val.parse().unwrap_or(config.limits.max_clients);
        }
        if let Ok(val) = std::env::var("RUSTYNAUT_LIMITS_MAX_MESSAGE_SIZE") {
            config.limits.max_message_size = val.parse().unwrap_or(config.limits.max_message_size);
        }

        // Logging
        if let Ok(val) = std::env::var("RUSTYNAUT_LOGGING_LEVEL") {
            config.logging.level = val;
        }
        if let Ok(val) = std::env::var("RUSTYNAUT_LOGGING_FORMAT") {
            config.logging.format = val;
        }

        config
    }

    /// Merge client config with environment variables.
    fn merge_client_from_env(mut config: ClientConfig) -> ClientConfig {
        // Connection
        if let Ok(val) = std::env::var("RUSTYNAUT_CONNECTION_BROKER_ADDRESS") {
            config.connection.broker_address = Some(val);
        }
        if let Ok(val) = std::env::var("RUSTYNAUT_CONNECTION_DEFAULT_USERNAME") {
            config.connection.default_username = Some(val);
        }
        if let Ok(val) = std::env::var("RUSTYNAUT_CONNECTION_DEFAULT_ROOM") {
            config.connection.default_room = Some(val);
        }

        // Reconnect
        if let Ok(val) = std::env::var("RUSTYNAUT_CONNECTION_RECONNECT_ENABLED") {
            config.connection.reconnect.enabled =
                val.parse().unwrap_or(config.connection.reconnect.enabled);
        }
        if let Ok(val) = std::env::var("RUSTYNAUT_CONNECTION_RECONNECT_MAX_ATTEMPTS") {
            config.connection.reconnect.max_attempts = val
                .parse()
                .unwrap_or(config.connection.reconnect.max_attempts);
        }
        if let Ok(val) = std::env::var("RUSTYNAUT_CONNECTION_RECONNECT_BASE_DELAY_SECONDS") {
            config.connection.reconnect.base_delay_seconds = val
                .parse()
                .unwrap_or(config.connection.reconnect.base_delay_seconds);
        }
        if let Ok(val) = std::env::var("RUSTYNAUT_CONNECTION_RECONNECT_MAX_DELAY_SECONDS") {
            config.connection.reconnect.max_delay_seconds = val
                .parse()
                .unwrap_or(config.connection.reconnect.max_delay_seconds);
        }

        // Clipboard
        if let Ok(val) = std::env::var("RUSTYNAUT_CLIPBOARD_ENABLED") {
            config.clipboard.enabled = val.parse().unwrap_or(config.clipboard.enabled);
        }
        if let Ok(val) = std::env::var("RUSTYNAUT_CLIPBOARD_FILE_DETECTION") {
            config.clipboard.file_detection =
                val.parse().unwrap_or(config.clipboard.file_detection);
        }
        if let Ok(val) = std::env::var("RUSTYNAUT_CLIPBOARD_AUTO_OFFER_FILES") {
            config.clipboard.auto_offer_files =
                val.parse().unwrap_or(config.clipboard.auto_offer_files);
        }

        // UI
        if let Ok(val) = std::env::var("RUSTYNAUT_UI_THEME") {
            config.ui.theme = val;
        }
        if let Ok(val) = std::env::var("RUSTYNAUT_UI_SHOW_TIMESTAMPS") {
            config.ui.show_timestamps = val.parse().unwrap_or(config.ui.show_timestamps);
        }
        if let Ok(val) = std::env::var("RUSTYNAUT_UI_MOUSE_ENABLED") {
            config.ui.mouse_enabled = val.parse().unwrap_or(config.ui.mouse_enabled);
        }

        // Logging
        if let Ok(val) = std::env::var("RUSTYNAUT_LOGGING_LEVEL") {
            config.logging.level = val;
        }

        config
    }

    /// Serialize config to TOML string for display.
    pub fn to_toml_string(config: &BrokerConfig) -> Result<String, ConfigError> {
        toml::to_string_pretty(config).map_err(ConfigError::serialize)
    }

    /// Serialize client config to TOML string for display.
    pub fn client_to_toml_string(config: &ClientConfig) -> Result<String, ConfigError> {
        toml::to_string_pretty(config).map_err(ConfigError::serialize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_default_broker_config() {
        let config = ConfigLoader::load_broker_config(None).unwrap();
        assert_eq!(config.server.bind_address, "0.0.0.0:4242");
    }

    #[test]
    fn test_load_default_client_config() {
        let config = ConfigLoader::load_client_config(None).unwrap();
        assert!(config.connection.reconnect.enabled);
        assert_eq!(config.connection.reconnect.max_attempts, 3);
    }
}
