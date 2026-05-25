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
    config_dir().map(|p| documented_broker_config_path(&p))
}

/// Get the client config file path.
pub fn client_config_path() -> Option<PathBuf> {
    config_dir().map(|p| documented_client_config_path(&p))
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

    find_existing_config(broker_config_candidates(
        config_dir().as_deref(),
        std::env::current_dir().ok().as_deref(),
    ))
}

/// Search for client config file.
pub fn find_client_config(explicit_path: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = explicit_path {
        return Some(PathBuf::from(path));
    }

    find_existing_config(client_config_candidates(
        config_dir().as_deref(),
        std::env::current_dir().ok().as_deref(),
    ))
}

fn documented_broker_config_path(config_root: &Path) -> PathBuf {
    config_root.join("broker").join("config.toml")
}

fn documented_client_config_path(config_root: &Path) -> PathBuf {
    config_root.join("client").join("config.toml")
}

fn legacy_broker_config_path(config_root: &Path) -> PathBuf {
    config_root.join("broker.toml")
}

fn legacy_client_config_path(config_root: &Path) -> PathBuf {
    config_root.join("client.toml")
}

fn broker_config_candidates(
    config_root: Option<&Path>,
    current_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(root) = config_root {
        paths.push(documented_broker_config_path(root));
        paths.push(legacy_broker_config_path(root));
    }

    if let Some(current) = current_dir {
        paths.push(current.join("broker.toml"));
    }

    paths
}

fn client_config_candidates(
    config_root: Option<&Path>,
    current_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(root) = config_root {
        paths.push(documented_client_config_path(root));
        paths.push(legacy_client_config_path(root));
    }

    if let Some(current) = current_dir {
        paths.push(current.join("client.toml"));
    }

    paths
}

fn find_existing_config(paths: Vec<PathBuf>) -> Option<PathBuf> {
    paths.into_iter().find(|path| path.exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn test_config_dir_exists() {
        let dir = config_dir();
        assert!(dir.is_some());
    }

    #[test]
    fn test_default_config_paths_use_documented_component_dirs() {
        assert!(broker_config_path()
            .unwrap()
            .ends_with(Path::new("rustynaut").join("broker").join("config.toml")));
        assert!(client_config_path()
            .unwrap()
            .ends_with(Path::new("rustynaut").join("client").join("config.toml")));
    }

    #[test]
    fn test_expand_path_tilde() {
        let path = PathBuf::from("~/test");
        let expanded = expand_path(&path);
        assert!(!expanded.to_string_lossy().starts_with("~"));
    }

    #[test]
    fn test_client_config_candidates_preserve_legacy_fallback_order() {
        let config_root = Path::new("/config/rustynaut");
        let current_dir = Path::new("/workspace");

        assert_eq!(
            client_config_candidates(Some(config_root), Some(current_dir)),
            vec![
                PathBuf::from("/config/rustynaut/client/config.toml"),
                PathBuf::from("/config/rustynaut/client.toml"),
                PathBuf::from("/workspace/client.toml"),
            ]
        );
    }

    #[test]
    fn test_broker_config_candidates_preserve_legacy_fallback_order() {
        let config_root = Path::new("/config/rustynaut");
        let current_dir = Path::new("/workspace");

        assert_eq!(
            broker_config_candidates(Some(config_root), Some(current_dir)),
            vec![
                PathBuf::from("/config/rustynaut/broker/config.toml"),
                PathBuf::from("/config/rustynaut/broker.toml"),
                PathBuf::from("/workspace/broker.toml"),
            ]
        );
    }

    #[test]
    fn test_existing_config_prefers_documented_path_over_legacy_path() {
        let root = unique_test_dir("config-paths");
        let documented = root.join("client").join("config.toml");
        let legacy = root.join("client.toml");

        fs::create_dir_all(documented.parent().unwrap()).unwrap();
        fs::write(&documented, "").unwrap();
        fs::write(&legacy, "").unwrap();

        let found = find_existing_config(client_config_candidates(Some(&root), None));

        fs::remove_dir_all(&root).unwrap();
        assert_eq!(found, Some(documented));
    }

    #[test]
    fn test_existing_config_falls_back_to_legacy_path() {
        let root = unique_test_dir("config-paths");
        let legacy = root.join("broker.toml");

        fs::create_dir_all(&root).unwrap();
        fs::write(&legacy, "").unwrap();

        let found = find_existing_config(broker_config_candidates(Some(&root), None));

        fs::remove_dir_all(&root).unwrap();
        assert_eq!(found, Some(legacy));
    }

    fn unique_test_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
    }
}
