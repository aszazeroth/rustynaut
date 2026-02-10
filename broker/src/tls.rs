//! TLS certificate generation and management for the Rustynaut broker.
//!
//! The broker acts as a Certificate Authority (CA), generating and signing
//! certificates for itself (server) and clients (via enrollment).

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType, PKCS_ECDSA_P256_SHA256,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use time::{Duration, OffsetDateTime};
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;

use rustynaut_common::tls::{
    cert_fingerprint, load_certs, load_private_key, save_pem, EnrolledCertBundle,
};

/// TLS configuration for the broker
pub struct TlsConfig {
    pub acceptor: TlsAcceptor,
    pub ca_cert_pem: String,
    pub ca_key: Arc<KeyPair>,
    pub enrollment_token: String,
    /// CA certificate path for display in TUI
    pub ca_cert_path: PathBuf,
}

/// Generate a CA certificate and keypair
fn generate_ca() -> Result<
    (rcgen::Certificate, CertificateParams, KeyPair),
    Box<dyn std::error::Error + Send + Sync>,
> {
    let mut params = CertificateParams::default();

    // Distinguished Name
    params
        .distinguished_name
        .push(DnType::CommonName, "Rustynaut CA");
    params
        .distinguished_name
        .push(DnType::OrganizationName, "Rustynaut");

    // Validity: 10 years, starting 1 hour ago for clock skew tolerance
    params.not_before = OffsetDateTime::now_utc() - Duration::hours(1);
    params.not_after = params.not_before + Duration::days(3650);

    // Mark as CA
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);

    // Key usages for CA
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];

    // Generate key pair
    let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;

    // Self-sign the CA certificate
    let cert = params.self_signed(&key_pair)?;

    Ok((cert, params, key_pair))
}

/// Generate a server certificate signed by the CA
fn generate_server_cert(
    ca_params: &CertificateParams,
    ca_key: &KeyPair,
    server_names: &[String],
) -> Result<(rcgen::Certificate, KeyPair), Box<dyn std::error::Error + Send + Sync>> {
    let mut params = CertificateParams::new(server_names.to_vec())?;

    // Distinguished Name
    params
        .distinguished_name
        .push(DnType::CommonName, "Rustynaut Broker");

    // Validity: 1 year, starting 1 hour ago for clock skew tolerance
    params.not_before = OffsetDateTime::now_utc() - Duration::hours(1);
    params.not_after = params.not_before + Duration::days(365);

    // Not a CA
    params.is_ca = IsCa::ExplicitNoCa;

    // Key usages for server
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];

    // Extended key usage: TLS server auth
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

    // Add IP addresses as SANs
    for name in server_names {
        if let Ok(ip) = name.parse::<std::net::IpAddr>() {
            params.subject_alt_names.push(SanType::IpAddress(ip));
        }
    }

    // Generate key pair
    let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;

    // Create issuer from CA params and key
    let issuer = Issuer::from_params(ca_params, ca_key);

    // Sign with CA using the issuer
    let cert = params.signed_by(&key_pair, &issuer)?;

    Ok((cert, key_pair))
}

/// Generate a client certificate signed by the CA
pub fn generate_client_cert(
    ca_cert_pem: &str,
    ca_key: &KeyPair,
    username: &str,
) -> Result<EnrolledCertBundle, Box<dyn std::error::Error + Send + Sync>> {
    let mut params = CertificateParams::default();

    // Distinguished Name - include username for identification
    params.distinguished_name.push(DnType::CommonName, username);
    params
        .distinguished_name
        .push(DnType::OrganizationName, "Rustynaut Client");

    // Validity: 1 year, starting 1 hour ago for clock skew tolerance
    params.not_before = OffsetDateTime::now_utc() - Duration::hours(1);
    params.not_after = params.not_before + Duration::days(365);

    // Not a CA
    params.is_ca = IsCa::ExplicitNoCa;

    // Key usages for client
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];

    // Extended key usage: TLS client auth
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];

    // Generate key pair for the client
    let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;

    // Create issuer from CA cert PEM
    let issuer = Issuer::from_ca_cert_pem(ca_cert_pem, ca_key)?;

    // Sign with CA
    let cert = params.signed_by(&key_pair, &issuer)?;

    Ok(EnrolledCertBundle {
        cert_pem: cert.pem(),
        key_pem: key_pair.serialize_pem(),
        ca_cert_pem: ca_cert_pem.to_string(),
    })
}

/// Load keypair from PEM file (for rcgen operations)
fn load_keypair(path: &Path) -> Result<KeyPair, Box<dyn std::error::Error + Send + Sync>> {
    let pem = fs::read_to_string(path)?;
    let key_pair = KeyPair::from_pem(&pem)?;
    Ok(key_pair)
}

/// Load or generate enrollment token
fn load_or_generate_token(
    cert_dir: &Path,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let token_path = cert_dir.join("enrollment-token");

    if token_path.exists() {
        let token = fs::read_to_string(&token_path)?.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    // Generate new token
    let token = Uuid::new_v4().to_string();

    // Create parent directory if needed
    if let Some(parent) = token_path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(&token_path, &token)?;

    // Set restrictive permissions (Unix)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600))?;
    }

    Ok(token)
}

/// Regenerate enrollment token
pub fn regenerate_token(
    cert_dir: &Path,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let token_path = cert_dir.join("enrollment-token");
    let token = Uuid::new_v4().to_string();

    if let Some(parent) = token_path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(&token_path, &token)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600))?;
    }

    Ok(token)
}

/// Initialize TLS configuration, generating certificates if needed
pub fn init_tls(
    cert_dir: &Path,
    server_names: &[String],
    regenerate_token_flag: bool,
) -> Result<TlsConfig, Box<dyn std::error::Error + Send + Sync>> {
    // Install ring as the crypto provider for rustls
    let _ = rustls::crypto::ring::default_provider().install_default();

    let ca_dir = cert_dir.join("ca");
    let broker_dir = cert_dir.join("broker");

    let ca_cert_path = ca_dir.join("ca.crt");
    let ca_key_path = ca_dir.join("ca.key");
    let server_cert_path = broker_dir.join("server.crt");
    let server_key_path = broker_dir.join("server.key");

    // Check if certs exist
    let certs_exist = ca_cert_path.exists()
        && ca_key_path.exists()
        && server_cert_path.exists()
        && server_key_path.exists();

    let (ca_cert_pem, ca_key) = if certs_exist {
        tracing::info!("Loading existing certificates from {:?}", cert_dir);

        let ca_cert_pem = fs::read_to_string(&ca_cert_path)?;
        let ca_key = load_keypair(&ca_key_path)?;

        (ca_cert_pem, ca_key)
    } else {
        tracing::info!("Generating new certificates in {:?}", cert_dir);

        // Generate CA
        let (ca_cert, ca_params, ca_key) = generate_ca()?;
        let ca_cert_pem = ca_cert.pem();
        let ca_key_pem = ca_key.serialize_pem();

        // Save CA
        save_pem(&ca_cert_pem, &ca_key_pem, &ca_cert_path, &ca_key_path)?;

        // Generate server cert
        let (server_cert, server_key) = generate_server_cert(&ca_params, &ca_key, server_names)?;
        let server_cert_pem = server_cert.pem();
        let server_key_pem = server_key.serialize_pem();

        // Save server cert
        save_pem(
            &server_cert_pem,
            &server_key_pem,
            &server_cert_path,
            &server_key_path,
        )?;

        (ca_cert_pem, ca_key)
    };

    // Log fingerprints
    let ca_certs = load_certs(&ca_cert_path)?;
    if let Some(ca_cert_der) = ca_certs.first() {
        tracing::info!("CA cert fingerprint: {}", cert_fingerprint(ca_cert_der));
    }

    let server_certs = load_certs(&server_cert_path)?;
    if let Some(server_cert_der) = server_certs.first() {
        tracing::info!(
            "Server cert fingerprint: {}",
            cert_fingerprint(server_cert_der)
        );
    }

    // Load or generate enrollment token
    let enrollment_token = if regenerate_token_flag {
        regenerate_token(cert_dir)?
    } else {
        load_or_generate_token(cert_dir)?
    };

    // Build TLS acceptor
    let server_certs = load_certs(&server_cert_path)?;
    let server_key = load_private_key(&server_key_path)?;

    // Load CA for client verification
    let mut client_auth_roots = rustls::RootCertStore::empty();
    for cert in &ca_certs {
        client_auth_roots.add(cert.clone())?;
    }

    // Create client verifier that allows unauthenticated clients (for enrollment)
    let client_verifier =
        rustls::server::WebPkiClientVerifier::builder(Arc::new(client_auth_roots))
            .allow_unauthenticated()
            .build()?;

    let config = rustls::ServerConfig::builder()
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(server_certs, server_key)?;

    let acceptor = TlsAcceptor::from(Arc::new(config));

    Ok(TlsConfig {
        acceptor,
        ca_cert_pem,
        ca_key: Arc::new(ca_key),
        enrollment_token,
        ca_cert_path,
    })
}
