use std::path::PathBuf;

pub fn base_cert_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rustynaut")
}

pub fn default_broker_cert_dir() -> PathBuf {
    base_cert_dir()
}

pub fn default_client_cert_dir() -> PathBuf {
    base_cert_dir().join("client")
}
