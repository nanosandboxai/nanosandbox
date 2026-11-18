//! Client error type.

use nanosb_protocol::{ErrorCode, ProtocolError};
use std::fmt;

/// A client-side error.
#[derive(Debug)]
pub enum Error {
    /// Could not connect to the helper/sandbox socket.
    Connect(String),
    /// Transport I/O failure.
    Io(std::io::Error),
    /// The helper returned a structured protocol error.
    Protocol(ProtocolError),
    /// The helper returned an unexpected frame.
    Unexpected(String),
    /// A required capability is missing on this helper build.
    Unsupported(String),
    /// A client-side wait/poll deadline elapsed.
    Timeout(String),
}

impl Error {
    pub fn connect(msg: impl Into<String>) -> Self {
        Error::Connect(msg.into())
    }

    pub fn unexpected(msg: impl Into<String>) -> Self {
        Error::Unexpected(msg.into())
    }

    /// The stable protocol error code, if this is a protocol error.
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            Error::Protocol(p) => Some(p.code),
            Error::Unsupported(_) => Some(ErrorCode::Unsupported),
            _ => None,
        }
    }

    /// True when the sandbox does not exist.
    pub fn is_not_found(&self) -> bool {
        self.code() == Some(ErrorCode::NotFound)
    }

    /// True when the per-sandbox endpoint is gone (a stopped sandbox has no
    /// supervisor socket), so the caller should retry through the helper.
    pub fn is_missing_endpoint(&self) -> bool {
        matches!(self, Error::Connect(_) | Error::Io(_))
            || self.code() == Some(ErrorCode::InvalidState)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Connect(m) => write!(f, "connect failed: {m}"),
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::Protocol(p) => write!(f, "{p}"),
            Error::Unexpected(m) => write!(f, "unexpected response: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
            Error::Timeout(m) => write!(f, "timed out: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<ProtocolError> for Error {
    fn from(p: ProtocolError) -> Self {
        Error::Protocol(p)
    }
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;
