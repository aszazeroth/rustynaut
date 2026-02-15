//! Configuration error types.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Failed to read config file: {path}")]
    FileRead {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("Failed to parse config file: {path}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },

    #[error("Failed to serialize config: {source}")]
    Serialize { source: toml::ser::Error },

    #[error("Invalid configuration value: {field} = {value}, {reason}")]
    InvalidValue {
        field: String,
        value: String,
        reason: String,
    },

    #[error("Missing required configuration: {field}")]
    MissingField { field: String },

    #[error("Config migration failed: {version} -> {target}: {reason}")]
    Migration {
        version: String,
        target: String,
        reason: String,
    },

    #[error("Environment variable error: {name}: {reason}")]
    EnvVar { name: String, reason: String },

    #[error("Config file not found: {path}")]
    NotFound { path: PathBuf },
}

impl ConfigError {
    pub fn file_read(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::FileRead {
            path: path.into(),
            source,
        }
    }

    pub fn parse(path: impl Into<PathBuf>, source: toml::de::Error) -> Self {
        Self::Parse {
            path: path.into(),
            source,
        }
    }

    pub fn serialize(source: toml::ser::Error) -> Self {
        Self::Serialize { source }
    }

    pub fn invalid_value(
        field: impl Into<String>,
        value: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::InvalidValue {
            field: field.into(),
            value: value.into(),
            reason: reason.into(),
        }
    }

    pub fn missing_field(field: impl Into<String>) -> Self {
        Self::MissingField {
            field: field.into(),
        }
    }

    pub fn migration(
        version: impl Into<String>,
        target: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::Migration {
            version: version.into(),
            target: target.into(),
            reason: reason.into(),
        }
    }

    pub fn env_var(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::EnvVar {
            name: name.into(),
            reason: reason.into(),
        }
    }

    pub fn not_found(path: impl Into<PathBuf>) -> Self {
        Self::NotFound { path: path.into() }
    }
}
