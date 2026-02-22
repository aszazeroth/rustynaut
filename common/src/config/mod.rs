//! Configuration management for Rustynaut.
//!
//! Provides unified configuration loading from multiple sources:
//! - Default values (hardcoded)
//! - Config file (TOML)
//! - Environment variables
//! - CLI arguments
//!
//! Platform-appropriate config paths:
//! - Linux: ~/.config/rustynaut/
//! - macOS: ~/Library/Application Support/rustynaut/
//! - Windows: %APPDATA%\rustynaut\

pub mod error;
pub mod load;
pub mod paths;
pub mod types;

pub use error::ConfigError;
pub use load::ConfigLoader;
pub use types::{BrokerConfig, ClientConfig, ReconnectConfig, UiConfig};
