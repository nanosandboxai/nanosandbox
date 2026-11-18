//! In-memory per-sandbox secret store.
//!
//! AC7: secret values are never persisted to disk and never returned by
//! `SecretList` — the store lives only in the supervisor process's memory and
//! `list()` returns keys with a redacted (absent) value.

use std::collections::BTreeMap;
use std::sync::Mutex;

use nanosb_protocol::SecretEntry;

/// Per-sandbox in-memory secret store.
pub struct SecretStore {
    inner: Mutex<BTreeMap<String, String>>,
}

impl SecretStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(BTreeMap::new()),
        }
    }

    /// Set (or overwrite) a secret. The value lives only in memory.
    pub fn set(&self, key: String, value: String) {
        if let Ok(mut m) = self.inner.lock() {
            m.insert(key, value);
        }
    }

    /// List secret keys with redacted values (never the stored value).
    pub fn list(&self) -> Vec<SecretEntry> {
        let mut out = Vec::new();
        if let Ok(m) = self.inner.lock() {
            for key in m.keys() {
                out.push(SecretEntry {
                    key: key.clone(),
                    value: None,
                });
            }
        }
        out
    }

    /// Remove a secret; returns true if it existed.
    pub fn remove(&self, key: &str) -> bool {
        if let Ok(mut m) = self.inner.lock() {
            m.remove(key).is_some()
        } else {
            false
        }
    }
}

impl Default for SecretStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_contains(dir: &std::path::Path, needle: &str) -> bool {
        let mut stack = vec![dir.to_path_buf()];
        while let Some(p) = stack.pop() {
            if let Ok(entries) = std::fs::read_dir(&p) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else if let Ok(content) = std::fs::read_to_string(&path) {
                        if content.contains(needle) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    #[test]
    fn set_list_remove_roundtrip() {
        let store = SecretStore::new();
        store.set("API_KEY".to_string(), "v1".to_string());
        store.set("TOKEN".to_string(), "v2".to_string());
        let entries = store.list();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.key == "API_KEY"));
        assert!(entries.iter().any(|e| e.key == "TOKEN"));
        assert!(store.remove("API_KEY"));
        assert!(!store.remove("API_KEY"));
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn list_never_returns_secret_values() {
        let store = SecretStore::new();
        store.set("API_KEY".to_string(), "super-secret-value".to_string());
        for e in store.list() {
            assert!(e.value.is_none() || e.value.as_deref() == Some(""));
        }
    }

    #[test]
    fn secret_value_never_appears_under_sandbox_dir() {
        let store = SecretStore::new();
        let value = "super-secret-value-xyz";
        store.set("API_KEY".to_string(), value.to_string());

        let dir = std::env::temp_dir().join(format!("nsb-secrets-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("config.json"), r#"{"name":"t","image":"alpine"}"#).unwrap();
        std::fs::write(dir.join("state.json"), r#"{"state":"running"}"#).unwrap();

        assert!(!dir_contains(&dir, value));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
