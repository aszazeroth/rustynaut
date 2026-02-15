//! Configuration types for broker and client.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const CONFIG_VERSION: &str = "0.1.0";

// ============================================================================
// Simple structs first (no dependencies on other config structs)
// ============================================================================

/// Logging configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct LoggingConfig {
    /// Log level: trace, debug, info, warn, error
    pub level: String,
    /// Log format: pretty, json, compact
    pub format: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: "pretty".to_string(),
        }
    }
}

/// Reconnection settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct ReconnectConfig {
    /// Enable auto-reconnection
    pub enabled: bool,
    /// Maximum reconnection attempts
    pub max_attempts: u32,
    /// Base delay in seconds
    pub base_delay_seconds: u64,
    /// Maximum delay cap in seconds
    pub max_delay_seconds: u64,
    /// Minimum connection duration to attempt reconnect (seconds)
    pub min_connection_seconds: u64,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_attempts: 3,
            base_delay_seconds: 1,
            max_delay_seconds: 8,
            min_connection_seconds: 5,
        }
    }
}

/// Client TLS settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct TlsClientConfig {
    /// Certificate directory
    pub cert_dir: PathBuf,
    /// Verify server certificate
    pub verify_server_cert: bool,
    /// CA certificate path override
    pub ca_cert_path: Option<PathBuf>,
}

impl Default for TlsClientConfig {
    fn default() -> Self {
        Self {
            cert_dir: dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("rustynaut")
                .join("client"),
            verify_server_cert: true,
            ca_cert_path: None,
        }
    }
}

/// Clipboard settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct ClipboardConfig {
    /// Clipboard sync enabled
    pub enabled: bool,
    /// Poll interval in milliseconds
    pub poll_interval_ms: u64,
    /// File detection enabled
    pub file_detection: bool,
    /// Auto-offer files
    pub auto_offer_files: bool,
    /// File size threshold in KB
    pub file_size_threshold_kb: u64,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_ms: 100,
            file_detection: true,
            auto_offer_files: true,
            file_size_threshold_kb: 64,
        }
    }
}

/// UI/TUI settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct UiConfig {
    /// Theme: default, dark, light
    pub theme: String,
    /// Show timestamps
    pub show_timestamps: bool,
    /// Timestamp format
    pub timestamp_format: String,
    /// Show user list
    pub show_user_list: bool,
    /// User list position: right, left, hidden
    pub user_list_position: String,
    /// Message wrap
    pub message_wrap: bool,
    /// Mouse enabled
    pub mouse_enabled: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "default".to_string(),
            show_timestamps: true,
            timestamp_format: "%H:%M:%S".to_string(),
            show_user_list: true,
            user_list_position: "right".to_string(),
            message_wrap: true,
            mouse_enabled: true,
        }
    }
}

/// Server binding configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct ServerConfig {
    /// Network address to bind to
    pub bind_address: String,
    /// Certificate directory
    pub cert_dir: PathBuf,
    /// Enrollment enabled
    pub enrollment_enabled: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_address: "0.0.0.0:4242".to_string(),
            cert_dir: dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("rustynaut"),
            enrollment_enabled: true,
        }
    }
}

/// Limits and quotas
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct LimitsConfig {
    /// Maximum concurrent clients
    pub max_clients: usize,
    /// Maximum rooms
    pub max_rooms: usize,
    /// Maximum clients per room
    pub max_clients_per_room: usize,
    /// Maximum message size in bytes
    pub max_message_size: usize,
    /// Maximum file size in bytes
    pub max_file_size: u64,
    /// Rate limit: messages per second
    pub rate_limit_messages_per_second: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_clients: 100,
            max_rooms: 50,
            max_clients_per_room: 50,
            max_message_size: 2 * 1024 * 1024, // 2MB
            max_file_size: 1024 * 1024 * 1024, // 1GB
            rate_limit_messages_per_second: 10,
        }
    }
}

/// Feature toggles
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct FeaturesConfig {
    /// Rooms enabled
    pub rooms_enabled: bool,
    /// File transfers enabled
    pub file_transfers: bool,
    /// Clipboard sync enabled
    pub clipboard_sync: bool,
}

impl Default for FeaturesConfig {
    fn default() -> Self {
        Self {
            rooms_enabled: true,
            file_transfers: true,
            clipboard_sync: true,
        }
    }
}

/// Timeout values in seconds
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct TimeoutsConfig {
    /// Client idle timeout
    pub client_idle_timeout_seconds: u64,
    /// File transfer timeout
    pub file_transfer_timeout_seconds: u64,
    /// Enrollment session timeout
    pub enrollment_session_timeout_seconds: u64,
}

impl Default for TimeoutsConfig {
    fn default() -> Self {
        Self {
            client_idle_timeout_seconds: 300,  // 5 minutes
            file_transfer_timeout_seconds: 60, // 1 minute
            enrollment_session_timeout_seconds: 30,
        }
    }
}

// ============================================================================
// Composite structs (reference simpler structs defined above)
// ============================================================================

/// Client connection settings
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case", default)]
pub struct ConnectionConfig {
    /// Default broker address
    pub broker_address: Option<String>,
    /// Default username
    pub default_username: Option<String>,
    /// Default room
    pub default_room: Option<String>,
    /// Reconnection settings
    pub reconnect: ReconnectConfig,
    /// TLS settings
    pub tls: TlsClientConfig,
}

/// Broker configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case", default)]
pub struct BrokerConfig {
    /// Server settings
    pub server: ServerConfig,
    /// Limits and quotas
    pub limits: LimitsConfig,
    /// Logging configuration
    pub logging: LoggingConfig,
    /// Feature toggles
    pub features: FeaturesConfig,
    /// Timeout values
    pub timeouts: TimeoutsConfig,
}

/// Client configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case", default)]
pub struct ClientConfig {
    /// Connection settings
    pub connection: ConnectionConfig,
    /// Clipboard settings
    pub clipboard: ClipboardConfig,
    /// UI/TUI settings
    pub ui: UiConfig,
    /// Logging configuration
    pub logging: LoggingConfig,
}
