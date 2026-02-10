use base64::{engine::general_purpose, Engine as _};

use crate::protocol::ENROLLED_PREFIX;

/// Certificate bundle exchanged during enrollment
pub struct EnrolledCertBundle {
    pub cert_pem: String,
    pub key_pem: String,
    pub ca_cert_pem: String,
}

/// Encode a client cert bundle for the ENROLLED response
pub fn encode_enrolled_response(bundle: &EnrolledCertBundle) -> String {
    let cert_b64 = general_purpose::STANDARD.encode(&bundle.cert_pem);
    let key_b64 = general_purpose::STANDARD.encode(&bundle.key_pem);
    let ca_b64 = general_purpose::STANDARD.encode(&bundle.ca_cert_pem);
    format!("{ENROLLED_PREFIX}{} {} {}", cert_b64, key_b64, ca_b64)
}

/// Parse the ENROLLED response from the broker
/// Format: ENROLLED <cert_b64> <key_b64> <ca_b64>
pub fn parse_enrolled_response(
    line: &str,
) -> Result<EnrolledCertBundle, Box<dyn std::error::Error + Send + Sync>> {
    let rest = line
        .strip_prefix(ENROLLED_PREFIX)
        .ok_or("Invalid ENROLLED response")?;

    let mut parts = rest.splitn(3, ' ');
    let cert_b64 = parts.next().ok_or("Missing cert in ENROLLED response")?;
    let key_b64 = parts.next().ok_or("Missing key in ENROLLED response")?;
    let ca_b64 = parts.next().ok_or("Missing CA in ENROLLED response")?;

    let cert_pem = String::from_utf8(general_purpose::STANDARD.decode(cert_b64)?)?;
    let key_pem = String::from_utf8(general_purpose::STANDARD.decode(key_b64)?)?;
    let ca_cert_pem = String::from_utf8(general_purpose::STANDARD.decode(ca_b64)?)?;

    Ok(EnrolledCertBundle {
        cert_pem,
        key_pem,
        ca_cert_pem,
    })
}
