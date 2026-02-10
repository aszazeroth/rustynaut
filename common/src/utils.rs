use base64::{engine::general_purpose, Engine as _};

use crate::error::RustynautError;

pub fn format_size(size: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if size >= GB {
        format!("{:.1} GB", size as f64 / GB as f64)
    } else if size >= MB {
        format!("{:.1} MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{:.1} KB", size as f64 / KB as f64)
    } else {
        format!("{} bytes", size)
    }
}

pub fn encode_base64<T: AsRef<[u8]>>(data: T) -> String {
    general_purpose::STANDARD.encode(data)
}

pub fn decode_base64(data: &str) -> Result<Vec<u8>, RustynautError> {
    Ok(general_purpose::STANDARD.decode(data)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_roundtrip() {
        let original = "Hello, World!\nLine 2";
        let encoded = encode_base64(original);
        let decoded_bytes = decode_base64(&encoded).unwrap();
        let decoded = String::from_utf8(decoded_bytes).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_base64_with_special_chars() {
        let original = "{\n  \"name\": \"test\",\n  \"value\": 123\n}";
        let encoded = encode_base64(original);
        let decoded_bytes = decode_base64(&encoded).unwrap();
        let decoded = String::from_utf8(decoded_bytes).unwrap();
        assert_eq!(original, decoded);
    }
}
