use std::error::Error;
use std::fmt::{Display, Formatter, Result as FmtResult};

#[derive(Debug)]
pub enum RustynautError {
    Base64(base64::DecodeError),
    Utf8(std::string::FromUtf8Error),
}

impl Display for RustynautError {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        match self {
            RustynautError::Base64(err) => write!(f, "base64 decode error: {err}"),
            RustynautError::Utf8(err) => write!(f, "utf8 decode error: {err}"),
        }
    }
}

impl Error for RustynautError {}

impl From<base64::DecodeError> for RustynautError {
    fn from(err: base64::DecodeError) -> Self {
        RustynautError::Base64(err)
    }
}

impl From<std::string::FromUtf8Error> for RustynautError {
    fn from(err: std::string::FromUtf8Error) -> Self {
        RustynautError::Utf8(err)
    }
}
