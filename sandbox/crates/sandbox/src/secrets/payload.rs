use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// A payload of secrets to be passed into a sandbox.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SecretPayload {
    /// Named secrets (key -> value).
    pub secrets: HashMap<String, String>,
    /// Intercepted file contents (original path -> contents).
    pub intercepted_files: HashMap<String, String>,
}

impl SecretPayload {
    /// Create a new empty payload.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a named secret.
    pub fn add_secret(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.secrets.insert(key.into(), value.into());
    }

    /// Add an intercepted file (original path -> contents).
    pub fn add_intercepted_file(&mut self, path: impl Into<String>, contents: impl Into<String>) {
        self.intercepted_files.insert(path.into(), contents.into());
    }

    /// Returns true if no secrets and no intercepted files are present.
    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty() && self.intercepted_files.is_empty()
    }

    /// Serialize to JSON bytes.
    pub fn to_json_bytes(&self) -> serde_json::Result<Vec<u8>> {
        serde_json::to_vec(self)
    }

    /// Deserialize from JSON bytes.
    pub fn from_json_bytes(bytes: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_payload() {
        let p = SecretPayload::new();
        assert!(p.is_empty());
        assert!(p.secrets.is_empty());
        assert!(p.intercepted_files.is_empty());
    }

    #[test]
    fn test_add_secret() {
        let mut p = SecretPayload::new();
        p.add_secret("API_KEY", "hunter2");
        assert!(!p.is_empty());
        assert_eq!(p.secrets.get("API_KEY").map(|s| s.as_str()), Some("hunter2"));
    }

    #[test]
    fn test_add_intercepted_file() {
        let mut p = SecretPayload::new();
        p.add_intercepted_file("/etc/secrets.conf", "token=abc");
        assert!(!p.is_empty());
        assert_eq!(
            p.intercepted_files.get("/etc/secrets.conf").map(|s| s.as_str()),
            Some("token=abc")
        );
    }

    #[test]
    fn test_json_roundtrip() {
        let mut p = SecretPayload::new();
        p.add_secret("KEY", "VALUE");
        p.add_intercepted_file("/path/to/file", "contents");

        let bytes = p.to_json_bytes().expect("serialize");
        let p2 = SecretPayload::from_json_bytes(&bytes).expect("deserialize");

        assert_eq!(p2.secrets.get("KEY").map(|s| s.as_str()), Some("VALUE"));
        assert_eq!(
            p2.intercepted_files.get("/path/to/file").map(|s| s.as_str()),
            Some("contents")
        );
    }

}
