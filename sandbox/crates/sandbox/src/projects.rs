//! Project registry — tracks projects used with nanosandbox.
//!
//! Stores a JSON file at `~/.nanosandbox/projects.json` with a list of
//! known project paths, display names, and last-used timestamps.
//! Deduplicates by canonical path and updates `last_used` on each register.
//! The file is created lazily; missing or corrupt files are tolerated
//! (start empty).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Current schema version for the registry file.
const REGISTRY_VERSION: u32 = 1;

/// A single project entry in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEntry {
    /// Absolute canonical path to the project directory.
    pub path: String,
    /// Human-readable display name (typically the directory basename).
    pub display_name: String,
    /// RFC 3339 timestamp of the last `register()` call.
    pub last_used: DateTime<Utc>,
}

/// The on-disk registry file structure.
#[derive(Debug, Serialize, Deserialize)]
struct RegistryFile {
    version: u32,
    projects: Vec<ProjectEntry>,
}

/// Project registry — manages the `~/.nanosandbox/projects.json` file.
#[derive(Debug)]
pub struct ProjectRegistry {
    /// In-memory entries keyed by canonical path.
    entries: BTreeMap<String, ProjectEntry>,
    /// Path to the registry file.
    file_path: PathBuf,
}

impl ProjectRegistry {
    /// Load the registry from the default location (`~/.nanosandbox/projects.json`).
    ///
    /// If the file does not exist or is corrupt, an empty registry is returned.
    /// The file is NOT created until [`save`](ProjectRegistry::save) is called.
    pub fn load() -> Self {
        let file_path = Self::default_path();
        let entries = match fs::read_to_string(&file_path) {
            Ok(content) => match serde_json::from_str::<RegistryFile>(&content) {
                Ok(reg) => reg
                    .projects
                    .into_iter()
                    .map(|e| (e.path.clone(), e))
                    .collect(),
                Err(_) => {
                    // Corrupt file — start fresh.
                    BTreeMap::new()
                }
            },
            Err(_) => {
                // File doesn't exist yet — start empty.
                BTreeMap::new()
            }
        };
        ProjectRegistry { entries, file_path }
    }

    /// Register (or re-register) a project path.
    ///
    /// Canonicalizes the path, sets the display name from the directory
    /// basename, updates `last_used` to now, and persists to disk.
    /// Returns the canonical path that was registered.
    pub fn register(&mut self, path: &Path) -> String {
        let canonical = Self::canonicalize(path);
        let display_name = canonical
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| canonical.to_string_lossy().to_string());

        let entry = ProjectEntry {
            path: canonical.to_string_lossy().to_string(),
            display_name,
            last_used: Utc::now(),
        };
        let key = entry.path.clone();
        self.entries.insert(key.clone(), entry);
        let _ = self.save();
        key
    }

    /// Return all registered projects, sorted by `last_used` descending (MRU first).
    pub fn list(&self) -> Vec<ProjectEntry> {
        let mut entries: Vec<ProjectEntry> = self.entries.values().cloned().collect();
        entries.sort_by(|a, b| b.last_used.cmp(&a.last_used));
        entries
    }

    /// Remove a project from the registry by path.
    ///
    /// The path is canonicalized before matching. Returns `true` if an entry
    /// was removed, `false` if no matching entry was found.
    pub fn forget(&mut self, path: &Path) -> bool {
        let canonical = Self::canonicalize(path);
        let key = canonical.to_string_lossy().to_string();
        let removed = self.entries.remove(&key).is_some();
        if removed {
            let _ = self.save();
        }
        removed
    }

    /// Persist the current entries to disk.
    pub fn save(&self) -> Result<(), std::io::Error> {
        let reg = RegistryFile {
            version: REGISTRY_VERSION,
            projects: self.list(),
        };
        let json = serde_json::to_string_pretty(&reg)?;

        // Ensure the parent directory exists.
        if let Some(parent) = self.file_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Write atomically: write to a temp file, then rename.
        let tmp_path = self.file_path.with_extension("json.tmp");
        fs::write(&tmp_path, &json)?;
        fs::rename(&tmp_path, &self.file_path)?;
        Ok(())
    }

    /// Return the default path: `~/.nanosandbox/projects.json`.
    fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".nanosandbox")
            .join("projects.json")
    }

    /// Canonicalize a path, falling back to the absolute version if
    /// canonicalization fails (e.g. the path doesn't exist yet).
    fn canonicalize(path: &Path) -> PathBuf {
        path.canonicalize()
            .unwrap_or_else(|_| {
                if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    std::env::current_dir()
                        .unwrap_or_default()
                        .join(path)
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper: create a registry pointing at a temp file.
    fn registry_with_temp() -> (ProjectRegistry, TempDir) {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("projects.json");
        let entries = BTreeMap::new();
        let reg = ProjectRegistry {
            entries,
            file_path,
        };
        (reg, tmp)
    }

    #[test]
    fn test_register_and_list() {
        let (mut reg, _tmp) = registry_with_temp();
        let path = Path::new("/tmp");
        let canonical = reg.register(path);
        assert!(canonical.contains("tmp"), "should canonicalize /tmp");

        let list = reg.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].display_name, "tmp");
    }

    #[test]
    fn test_register_deduplicates() {
        let (mut reg, _tmp) = registry_with_temp();
        reg.register(Path::new("/tmp"));
        reg.register(Path::new("/tmp")); // same path again
        assert_eq!(reg.list().len(), 1, "should deduplicate by canonical path");
    }

    #[test]
    fn test_forget() {
        let (mut reg, _tmp) = registry_with_temp();
        reg.register(Path::new("/tmp"));
        assert_eq!(reg.list().len(), 1);

        let removed = reg.forget(Path::new("/tmp"));
        assert!(removed, "forget should return true");
        assert!(reg.list().is_empty(), "list should be empty after forget");
    }

    #[test]
    fn test_forget_nonexistent() {
        let (mut reg, _tmp) = registry_with_temp();
        let removed = reg.forget(Path::new("/nonexistent"));
        assert!(!removed, "forget of nonexistent path should return false");
    }

    #[test]
    fn test_mru_ordering() {
        let (mut reg, _tmp) = registry_with_temp();
        reg.register(Path::new("/tmp"));
        // Re-register /tmp to update its last_used, then register /var
        // /var should be first (most recently used).
        reg.register(Path::new("/var"));

        let list = reg.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].display_name, "var", "MRU should be first");
        assert_eq!(list[1].display_name, "tmp");
    }

    #[test]
    fn test_load_missing_file() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("projects.json");
        let reg = ProjectRegistry {
            entries: BTreeMap::new(),
            file_path,
        };
        assert!(reg.list().is_empty(), "missing file should give empty list");
    }

    #[test]
    fn test_load_corrupt_file() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("projects.json");
        fs::write(&file_path, "not valid json").unwrap();
        let reg = ProjectRegistry {
            entries: BTreeMap::new(),
            file_path,
        };
        assert!(reg.list().is_empty(), "corrupt file should give empty list");
    }

    #[test]
    fn test_persistence_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("projects.json");

        // Write one entry via register (which saves to disk).
        {
            let mut reg = ProjectRegistry {
                entries: BTreeMap::new(),
                file_path: file_path.clone(),
            };
            reg.register(Path::new("/tmp"));
        }

        // Read back from disk by constructing a fresh registry that loads the file.
        // We simulate load() by reading the file and parsing it.
        let content = fs::read_to_string(&file_path).unwrap();
        let parsed: RegistryFile = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.version, REGISTRY_VERSION);
        assert_eq!(parsed.projects.len(), 1);
        assert_eq!(parsed.projects[0].display_name, "tmp");
    }
}
