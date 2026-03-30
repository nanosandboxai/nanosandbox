//! Registry Authentication
//!
//! Provides credential management for OCI registries using Docker config.json.

use crate::error::{Error, Result};
use oci_distribution::secrets::RegistryAuth;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, warn};

/// Docker config.json structure
#[derive(Debug, Default, serde::Deserialize)]
struct DockerConfig {
    #[serde(default)]
    auths: HashMap<String, AuthEntry>,
    #[serde(rename = "credsStore", default)]
    creds_store: Option<String>,
    #[serde(rename = "credHelpers", default)]
    cred_helpers: HashMap<String, String>,
}

/// Auth entry in Docker config
#[derive(Debug, serde::Deserialize)]
struct AuthEntry {
    auth: Option<String>,
    username: Option<String>,
    password: Option<String>,
}

/// Credential store for registry authentication
#[derive(Debug)]
pub struct CredentialStore {
    /// Parsed credentials by registry
    credentials: HashMap<String, (String, String)>,
    /// Default credential helper
    default_helper: Option<String>,
    /// Per-registry credential helpers
    cred_helpers: HashMap<String, String>,
}

impl CredentialStore {
    /// Create an empty credential store
    pub fn empty() -> Self {
        Self {
            credentials: HashMap::new(),
            default_helper: None,
            cred_helpers: HashMap::new(),
        }
    }

    /// Load credentials from default Docker config locations
    pub fn load() -> Result<Self> {
        let config_paths = Self::default_config_paths();
        Self::load_from_paths(&config_paths)
    }

    /// Load credentials from specific paths
    pub fn load_from_paths(paths: &[PathBuf]) -> Result<Self> {
        let mut credentials = HashMap::new();
        let mut default_helper = None;
        let mut cred_helpers = HashMap::new();

        for path in paths {
            if path.exists() {
                debug!("Loading Docker config from {:?}", path);
                match Self::parse_config(path) {
                    Ok(config) => {
                        // Merge auths
                        for (registry, auth) in config.auths {
                            if let Some((user, pass)) = Self::decode_auth(&auth) {
                                credentials
                                    .insert(Self::normalize_registry(&registry), (user, pass));
                            }
                        }
                        // Use first found helper
                        if default_helper.is_none() {
                            default_helper = config.creds_store;
                        }
                        // Merge cred helpers
                        for (registry, helper) in config.cred_helpers {
                            cred_helpers.insert(Self::normalize_registry(&registry), helper);
                        }
                    }
                    Err(e) => {
                        warn!("Failed to parse Docker config {:?}: {}", path, e);
                    }
                }
            }
        }

        Ok(Self {
            credentials,
            default_helper,
            cred_helpers,
        })
    }

    /// Get default config file paths
    fn default_config_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // XDG_RUNTIME_DIR/containers/auth.json (Linux)
        if let Some(runtime_dir) = dirs::runtime_dir() {
            paths.push(runtime_dir.join("containers").join("auth.json"));
        }

        // ~/.config/containers/auth.json
        if let Some(config_dir) = dirs::config_dir() {
            paths.push(config_dir.join("containers").join("auth.json"));
        }

        // ~/.docker/config.json (Docker default)
        if let Some(home) = dirs::home_dir() {
            paths.push(home.join(".docker").join("config.json"));
        }

        paths
    }

    /// Parse a Docker config file
    fn parse_config(path: &PathBuf) -> Result<DockerConfig> {
        let content = std::fs::read_to_string(path)?;
        let config: DockerConfig = serde_json::from_str(&content)?;
        Ok(config)
    }

    /// Decode base64 auth string to (username, password)
    fn decode_auth(auth: &AuthEntry) -> Option<(String, String)> {
        // Try username/password fields first
        if let (Some(user), Some(pass)) = (&auth.username, &auth.password) {
            return Some((user.clone(), pass.clone()));
        }

        // Try base64-encoded auth field
        if let Some(auth_str) = &auth.auth {
            if let Ok(decoded) = base64_decode(auth_str) {
                if let Some((user, pass)) = decoded.split_once(':') {
                    return Some((user.to_string(), pass.to_string()));
                }
            }
        }

        None
    }

    /// Normalize registry name for lookup
    fn normalize_registry(registry: &str) -> String {
        let registry = registry.trim_start_matches("https://");
        let registry = registry.trim_start_matches("http://");
        let registry = registry.trim_end_matches('/');

        // Handle Docker Hub special case
        match registry {
            "index.docker.io" | "index.docker.io/v1" | "registry-1.docker.io" => {
                "docker.io".to_string()
            }
            other => other.to_string(),
        }
    }

    /// Get authentication for a registry
    pub fn get_auth(&self, registry: &str) -> RegistryAuth {
        let normalized = Self::normalize_registry(registry);

        // Check if there's a credential helper for this registry
        if let Some(helper) = self.cred_helpers.get(&normalized) {
            if let Some(auth) = self.get_from_helper(helper, &normalized) {
                return auth;
            }
        }

        // Check if there's a default credential helper
        if let Some(helper) = &self.default_helper {
            if let Some(auth) = self.get_from_helper(helper, &normalized) {
                return auth;
            }
        }

        // Check stored credentials
        if let Some((user, pass)) = self.credentials.get(&normalized) {
            debug!("Found credentials for registry: {}", normalized);
            return RegistryAuth::Basic(user.clone(), pass.clone());
        }

        // Try matching with registry prefix (e.g., ghcr.io matches ghcr.io/user)
        for (stored_registry, (user, pass)) in &self.credentials {
            if normalized.starts_with(stored_registry) {
                debug!(
                    "Found prefix-matched credentials for registry: {}",
                    normalized
                );
                return RegistryAuth::Basic(user.clone(), pass.clone());
            }
        }

        debug!("No credentials found for registry: {}", normalized);
        RegistryAuth::Anonymous
    }

    /// Get credentials from a credential helper
    fn get_from_helper(&self, _helper: &str, registry: &str) -> Option<RegistryAuth> {
        debug!("Attempting to get credentials for registry: {}", registry);

        // Use docker_credential crate to get credentials
        // It automatically handles the credential helper based on config.json
        match docker_credential::get_credential(registry) {
            Ok(cred) => {
                debug!("Got credentials for {}", registry);
                match cred {
                    docker_credential::DockerCredential::UsernamePassword(user, pass) => {
                        Some(RegistryAuth::Basic(user, pass))
                    }
                    docker_credential::DockerCredential::IdentityToken(token) => {
                        // Identity tokens are typically used as password with empty username
                        Some(RegistryAuth::Basic(String::new(), token))
                    }
                }
            }
            Err(e) => {
                debug!("Credential retrieval failed for {}: {:?}", registry, e);
                None
            }
        }
    }

    /// Check if credentials exist for a registry
    pub fn has_credentials(&self, registry: &str) -> bool {
        let normalized = Self::normalize_registry(registry);
        self.credentials.contains_key(&normalized)
            || self.cred_helpers.contains_key(&normalized)
            || self.default_helper.is_some()
    }

    /// Add credentials manually
    pub fn add_credentials(&mut self, registry: &str, username: String, password: String) {
        let normalized = Self::normalize_registry(registry);
        self.credentials.insert(normalized, (username, password));
    }

    /// List registries with stored credentials
    pub fn list_registries(&self) -> Vec<&str> {
        self.credentials.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for CredentialStore {
    fn default() -> Self {
        Self::load().unwrap_or_else(|_| Self::empty())
    }
}

/// Decode base64 string
fn base64_decode(input: &str) -> std::result::Result<String, Error> {
    // Simple base64 decode without external crate
    let bytes: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();

    let decoded = base64_decode_bytes(&bytes)
        .map_err(|e| Error::InvalidConfig(format!("Base64 decode error: {}", e)))?;

    String::from_utf8(decoded)
        .map_err(|e| Error::InvalidConfig(format!("UTF-8 decode error: {}", e)))
}

/// Simple base64 decoder
fn base64_decode_bytes(input: &[u8]) -> std::result::Result<Vec<u8>, &'static str> {
    const DECODE_TABLE: [i8; 128] = [
        -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
        -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, 62, -1, -1,
        -1, 63, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, -1, -1, -1, -1, -1, -1, -1, 0, 1, 2, 3, 4,
        5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, -1, -1, -1,
        -1, -1, -1, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45,
        46, 47, 48, 49, 50, 51, -1, -1, -1, -1, -1,
    ];

    let mut output = Vec::with_capacity(input.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u8;

    for &byte in input {
        if byte == b'=' {
            break;
        }
        if byte >= 128 {
            return Err("Invalid base64 character");
        }
        let value = DECODE_TABLE[byte as usize];
        if value < 0 {
            return Err("Invalid base64 character");
        }

        buffer = (buffer << 6) | (value as u32);
        bits += 6;

        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_empty_credential_store() {
        let store = CredentialStore::empty();
        assert_eq!(store.get_auth("ghcr.io"), RegistryAuth::Anonymous);
    }

    #[test]
    fn test_normalize_registry() {
        assert_eq!(
            CredentialStore::normalize_registry("https://ghcr.io/"),
            "ghcr.io"
        );
        assert_eq!(
            CredentialStore::normalize_registry("index.docker.io/v1"),
            "docker.io"
        );
        assert_eq!(
            CredentialStore::normalize_registry("registry-1.docker.io"),
            "docker.io"
        );
    }

    #[test]
    fn test_base64_decode() {
        // "user:pass" encoded
        let encoded = "dXNlcjpwYXNz";
        let decoded = base64_decode(encoded).unwrap();
        assert_eq!(decoded, "user:pass");
    }

    #[test]
    fn test_add_credentials() {
        let mut store = CredentialStore::empty();
        store.add_credentials("ghcr.io", "user".to_string(), "token".to_string());

        assert!(store.has_credentials("ghcr.io"));
        match store.get_auth("ghcr.io") {
            RegistryAuth::Basic(user, pass) => {
                assert_eq!(user, "user");
                assert_eq!(pass, "token");
            }
            _ => panic!("Expected Basic auth"),
        }
    }

    #[test]
    fn test_load_docker_config() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");

        // Write test config with base64-encoded "testuser:testpass"
        std::fs::write(
            &config_path,
            r#"{
            "auths": {
                "ghcr.io": {
                    "auth": "dGVzdHVzZXI6dGVzdHBhc3M="
                }
            }
        }"#,
        )
        .unwrap();

        let store = CredentialStore::load_from_paths(&[config_path]).unwrap();

        match store.get_auth("ghcr.io") {
            RegistryAuth::Basic(user, pass) => {
                assert_eq!(user, "testuser");
                assert_eq!(pass, "testpass");
            }
            _ => panic!("Expected Basic auth"),
        }
    }
}
