//! TLS client support for Rustynaut.
//!
//! Handles certificate storage, enrollment, and TLS connection setup.

use rustls::pki_types::{CertificateDer, ServerName};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, TlsConnector};

use rustynaut_common::tls::{cert_fingerprint, load_certs, load_private_key, EnrolledCertBundle};

/// TLS configuration for the client
pub struct TlsClientConfig {
    pub connector: TlsConnector,
}


/// Check if client is enrolled (has valid certificates)
pub fn is_enrolled(cert_dir: &Path) -> bool {
    let cert_path = cert_dir.join("client.crt");
    let key_path = cert_dir.join("client.key");
    let ca_path = cert_dir.join("ca.crt");

    cert_path.exists() && key_path.exists() && ca_path.exists()
}

/// Clear existing certificates (for re-enrollment)
pub fn clear_certs(cert_dir: &Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cert_path = cert_dir.join("client.crt");
    let key_path = cert_dir.join("client.key");
    let ca_path = cert_dir.join("ca.crt");

    // Remove files if they exist (ignore errors if they don't)
    let _ = fs::remove_file(&cert_path);
    let _ = fs::remove_file(&key_path);
    let _ = fs::remove_file(&ca_path);

    Ok(())
}

/// Save the certificate bundle received during enrollment
pub fn save_enrolled_certs(
    cert_dir: &Path,
    bundle: &EnrolledCertBundle,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    fs::create_dir_all(cert_dir)?;

    let cert_path = cert_dir.join("client.crt");
    let key_path = cert_dir.join("client.key");
    let ca_path = cert_dir.join("ca.crt");

    fs::write(&cert_path, &bundle.cert_pem)?;
    fs::write(&key_path, &bundle.key_pem)?;
    fs::write(&ca_path, &bundle.ca_cert_pem)?;

    // Set restrictive permissions on key file (Unix)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

/// Initialize TLS for enrollment (no client cert, accepts any server cert)
pub fn init_tls_for_enrollment() -> Result<TlsClientConfig, Box<dyn std::error::Error + Send + Sync>>
{
    // Install ring as the crypto provider for rustls
    let _ = rustls::crypto::ring::default_provider().install_default();

    // For enrollment, we accept any server certificate since we don't have the CA yet
    // This is only used for initial enrollment
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(InsecureServerCertVerifier {}))
        .with_no_client_auth();

    let connector = TlsConnector::from(Arc::new(config));

    Ok(TlsClientConfig { connector })
}

/// Initialize TLS with client certificate (for authenticated connections)
pub fn init_tls_with_client_cert(
    cert_dir: &Path,
) -> Result<TlsClientConfig, Box<dyn std::error::Error + Send + Sync>> {
    // Install ring as the crypto provider for rustls
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cert_path = cert_dir.join("client.crt");
    let key_path = cert_dir.join("client.key");
    let ca_path = cert_dir.join("ca.crt");

    // Load CA for server verification
    let ca_certs = load_certs(&ca_path)?;
    let mut root_store = rustls::RootCertStore::empty();
    for cert in &ca_certs {
        root_store.add(cert.clone())?;
    }

    // Log CA fingerprint
    if let Some(ca_cert_der) = ca_certs.first() {
        eprintln!("CA fingerprint: {}", cert_fingerprint(ca_cert_der));
    }

    // Load client certificate and key
    let client_certs = load_certs(&cert_path)?;
    let client_key = load_private_key(&key_path)?;

    // Log client cert fingerprint
    if let Some(client_cert_der) = client_certs.first() {
        eprintln!(
            "Client cert fingerprint: {}",
            cert_fingerprint(client_cert_der)
        );
    }

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_client_auth_cert(client_certs, client_key)?;

    let connector = TlsConnector::from(Arc::new(config));

    Ok(TlsClientConfig { connector })
}

/// The fixed hostname used for TLS verification (matches broker certificate SAN)
pub const TLS_SERVER_NAME: &str = "rustynaut.local";

/// Connect with TLS
pub async fn connect_tls(
    connector: &TlsConnector,
    stream: TcpStream,
    server_addr: &str,
    log_to_stderr: bool,
) -> Result<TlsStream<TcpStream>, Box<dyn std::error::Error + Send + Sync>> {
    // For IP addresses, use rustynaut.local (which is in the broker's certificate SANs)
    // For hostnames, use the provided hostname
    let sni_name = if server_addr.parse::<std::net::IpAddr>().is_ok() {
        TLS_SERVER_NAME.to_string()
    } else {
        server_addr.to_string()
    };

    let server_name = ServerName::try_from(sni_name.clone())?;

    if log_to_stderr {
        eprintln!("TLS: Connecting with SNI name: {}", sni_name);
    }

    let tls_stream = connector.connect(server_name, stream).await.map_err(|e| {
        if log_to_stderr {
            eprintln!("TLS handshake error: {:?}", e);
        }
        e
    })?;

    if log_to_stderr {
        eprintln!("TLS: Handshake complete");
    }
    Ok(tls_stream)
}

/// Custom certificate verifier that accepts any certificate (for enrollment only)
#[derive(Debug)]
struct InsecureServerCertVerifier;

impl rustls::client::danger::ServerCertVerifier for InsecureServerCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        // Log the server certificate fingerprint for manual verification
        eprintln!("WARNING: Accepting unverified server certificate (enrollment mode)");
        eprintln!("Server cert fingerprint: {}", cert_fingerprint(end_entity));
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}
