//! Platform-specific configuration path utilities.

use std::path::{Path, PathBuf};

/// Application name for path resolution
pub const APP_NAME: &str = "rustynaut";

/// Get the platform-appropriate config directory.
///
/// - Linux: ~/.config/rustynaut
/// - macOS: ~/Library/Application Support/rustynaut
/// - Windows: %APPDATA%\rustynaut
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join(APP_NAME))
}

/// Get the broker config file path.
pub fn broker_config_path() -> Option<PathBuf> {
    config_dir().map(|p| p.join("broker.toml"))
}

/// Get the client config file path.
pub fn client_config_path() -> Option<PathBuf> {
    config_dir().map(|p| p.join("client.toml"))
}

/// Get the default certificate directory.
pub fn default_cert_dir() -> PathBuf {
    config_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Get the client certificate directory.
pub fn default_client_cert_dir() -> PathBuf {
    config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("client")
}

/// Expand ~ in path to home directory.
pub fn expand_path(path: &Path) -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        if path.starts_with("~") {
            let mut new_path = home;
            for component in path.components().skip(1) {
                new_path = new_path.join(component);
            }
            return new_path;
        }
    }
    path.to_path_buf()
}

/// Search for config file in multiple locations.
///
/// Order of precedence:
/// 1. Explicit path provided
/// 2. RUSTYNAUT_CONFIG env var
/// 3. Platform-specific config directory
/// 4. Current directory (development convenience)
pub fn find_config_file(explicit_path: Option<&str>) -> Option<PathBuf> {
    // 1. Explicit path
    if let Some(path) = explicit_path {
        return Some(PathBuf::from(path));
    }

    // 2. Environment variable
    if let Ok(env_path) = std::env::var("RUSTYNAUT_CONFIG") {
        return Some(PathBuf::from(env_path));
    }

    // 3. Platform-specific directory (already checked above)
    // 4. Current directory (development)
    let current_dir = std::env::current_dir()
        .ok()
        .map(|p| p.join("rustynaut.toml"));

    if let Some(path) = current_dir {
        if path.exists() {
            return Some(path);
        }
    }

    None
}

/// Search for broker config file.
pub fn find_broker_config(explicit_path: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = explicit_path {
        return Some(PathBuf::from(path));
    }

    // Check platform config directory
    if let Some(path) = broker_config_path() {
        if path.exists() {
            return Some(path);
        }
    }

    // Check current directory
    let current = std::env::current_dir().ok().map(|p| p.join("broker.toml"));

    if let Some(path) = current {
        if path.exists() {
            return Some(path);
        }
    }

    None
}

/// Search for client config file.
pub fn find_client_config(explicit_path: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = explicit_path {
        return Some(PathBuf::from(path));
    }

    // Check platform config directory
    if let Some(path) = client_config_path() {
        if path.exists() {
            return Some(path);
        }
    }

    // Check current directory
    let current = std::env::current_dir().ok().map(|p| p.join("client.toml"));

    if let Some(path) = current {
        if path.exists() {
            return Some(path);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_dir_exists() {
        let dir = config_dir();
        assert!(dir.is_some());
    }

    #[test]
    fn test_expand_path_tilde() {
        let path = PathBuf::from("~/test");
        let expanded = expand_path(&path);
        assert!(!expanded.to_string_lossy().starts_with("~"));
    }
}
