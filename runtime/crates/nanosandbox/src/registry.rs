//! Sandbox Registry
//!
//! Provides persistent storage for sandbox metadata, allowing recovery
//! of sandbox state after process restarts.

use crate::config::SandboxConfig;
use crate::error::{Error, Result};
use crate::sandbox::SandboxStatus;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// Information about a registered sandbox
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxInfo {
    /// Unique identifier
    pub id: String,
    /// Sandbox name
    pub name: String,
    /// Image used
    pub image: String,
    /// Current status
    pub status: SandboxStatus,
    /// Path to the OCI bundle
    pub bundle_path: PathBuf,
    /// Creation timestamp
    pub created_at: DateTime<Utc>,
    /// Last updated timestamp
    pub updated_at: DateTime<Utc>,
    /// Full configuration
    pub config: SandboxConfig,
}

/// Registry for tracking sandbox instances
pub struct SandboxRegistry {
    /// Directory for storing sandbox state files
    state_dir: PathBuf,
}

impl SandboxRegistry {
    /// Create a new registry with default state directory
    pub fn new() -> Result<Self> {
        let state_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".nanosandbox")
            .join("sandboxes");

        Self::with_state_dir(state_dir)
    }

    /// Create a new registry with custom state directory
    pub fn with_state_dir(state_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&state_dir)?;
        debug!("Sandbox registry initialized at {:?}", state_dir);
        Ok(Self { state_dir })
    }

    /// Get the state directory
    pub fn state_dir(&self) -> &PathBuf {
        &self.state_dir
    }

    /// Get the path for a sandbox's state file
    fn state_file(&self, id: &str) -> PathBuf {
        self.state_dir.join(format!("{}.json", id))
    }

    /// Register a sandbox
    pub fn register(&self, info: &SandboxInfo) -> Result<()> {
        let path = self.state_file(&info.id);
        let content = serde_json::to_string_pretty(info)?;
        fs::write(&path, content)?;
        debug!("Registered sandbox {} at {:?}", info.id, path);
        Ok(())
    }

    /// Update a sandbox's status
    pub fn update_status(&self, id: &str, status: SandboxStatus) -> Result<()> {
        let mut info = self
            .get(id)?
            .ok_or_else(|| Error::SandboxNotFound(id.to_string()))?;

        info.status = status;
        info.updated_at = Utc::now();
        self.register(&info)
    }

    /// Unregister a sandbox
    pub fn unregister(&self, id: &str) -> Result<()> {
        let path = self.state_file(id);
        if path.exists() {
            fs::remove_file(&path)?;
            debug!("Unregistered sandbox {}", id);
        }
        Ok(())
    }

    /// Get a sandbox by ID
    pub fn get(&self, id: &str) -> Result<Option<SandboxInfo>> {
        let path = self.state_file(id);
        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&path)?;
        let info: SandboxInfo = serde_json::from_str(&content)?;
        Ok(Some(info))
    }

    /// Check if a sandbox exists
    pub fn exists(&self, id: &str) -> bool {
        self.state_file(id).exists()
    }

    /// List all registered sandboxes
    pub fn list(&self) -> Result<Vec<SandboxInfo>> {
        let mut sandboxes = Vec::new();

        if !self.state_dir.exists() {
            return Ok(sandboxes);
        }

        for entry in fs::read_dir(&self.state_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "json") {
                match fs::read_to_string(&path) {
                    Ok(content) => match serde_json::from_str::<SandboxInfo>(&content) {
                        Ok(info) => sandboxes.push(info),
                        Err(e) => {
                            warn!("Failed to parse sandbox state {:?}: {}", path, e);
                        }
                    },
                    Err(e) => {
                        warn!("Failed to read sandbox state {:?}: {}", path, e);
                    }
                }
            }
        }

        // Sort by creation time (newest first)
        sandboxes.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        Ok(sandboxes)
    }

    /// List sandboxes with a specific status
    pub fn list_by_status(&self, status: SandboxStatus) -> Result<Vec<SandboxInfo>> {
        let all = self.list()?;
        Ok(all.into_iter().filter(|s| s.status == status).collect())
    }

    /// Clean up stale sandbox entries (those without bundles)
    pub fn cleanup_stale(&self) -> Result<usize> {
        let mut removed = 0;

        for info in self.list()? {
            if !info.bundle_path.exists() {
                info!("Cleaning up stale sandbox entry: {}", info.id);
                self.unregister(&info.id)?;
                removed += 1;
            }
        }

        if removed > 0 {
            info!("Cleaned up {} stale sandbox entries", removed);
        }

        Ok(removed)
    }

    /// Clean up orphaned bundle directories (bundles without registry entries).
    ///
    /// Catches the reverse case from `cleanup_stale()`: when a process crashes
    /// between bundle creation and sandbox registration, the bundle directory
    /// (potentially GBs) leaks on disk with no registry entry.
    /// Only removes bundles older than 1 hour to avoid racing with in-flight creation.
    pub fn cleanup_orphaned_bundles(&self, bundles_dir: &std::path::Path) -> Result<usize> {
        if !bundles_dir.exists() {
            return Ok(0);
        }

        let registered_ids: std::collections::HashSet<String> =
            self.list()?.into_iter().map(|s| s.id).collect();

        let one_hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        let mut removed = 0;

        for entry in fs::read_dir(bundles_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }

            let name = entry.file_name().to_string_lossy().to_string();
            if registered_ids.contains(&name) {
                continue;
            }

            // Only remove if older than 1 hour (safety margin for in-flight creation)
            let is_old = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|mtime| mtime < one_hour_ago)
                .unwrap_or(false);

            if !is_old {
                debug!(
                    "Skipping recent orphaned bundle: {} (less than 1 hour old)",
                    name
                );
                continue;
            }

            info!("Cleaning up orphaned bundle: {}", name);
            if let Err(e) = fs::remove_dir_all(entry.path()) {
                warn!("Failed to remove orphaned bundle {}: {}", name, e);
            } else {
                removed += 1;
            }
        }

        if removed > 0 {
            info!("Cleaned up {} orphaned bundles", removed);
        }

        Ok(removed)
    }

    /// Get count of sandboxes by status
    pub fn count_by_status(&self) -> Result<std::collections::HashMap<SandboxStatus, usize>> {
        let mut counts = std::collections::HashMap::new();

        for info in self.list()? {
            *counts.entry(info.status).or_insert(0) += 1;
        }

        Ok(counts)
    }
}

impl Default for SandboxRegistry {
    fn default() -> Self {
        Self::new().expect("Failed to create default registry")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_info(id: &str) -> SandboxInfo {
        SandboxInfo {
            id: id.to_string(),
            name: format!("test-{}", id),
            image: "alpine:latest".to_string(),
            status: SandboxStatus::Ready,
            bundle_path: PathBuf::from("/tmp/bundle"),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            config: SandboxConfig::builder()
                .name("test")
                .image("alpine:latest")
                .build(),
        }
    }

    #[test]
    fn test_registry_register_and_get() {
        let temp_dir = TempDir::new().unwrap();
        let registry = SandboxRegistry::with_state_dir(temp_dir.path().to_path_buf()).unwrap();

        let info = create_test_info("test-1");
        registry.register(&info).unwrap();

        let retrieved = registry.get("test-1").unwrap().unwrap();
        assert_eq!(retrieved.id, "test-1");
        assert_eq!(retrieved.name, "test-test-1");
    }

    #[test]
    fn test_registry_list() {
        let temp_dir = TempDir::new().unwrap();
        let registry = SandboxRegistry::with_state_dir(temp_dir.path().to_path_buf()).unwrap();

        registry.register(&create_test_info("test-1")).unwrap();
        registry.register(&create_test_info("test-2")).unwrap();
        registry.register(&create_test_info("test-3")).unwrap();

        let list = registry.list().unwrap();
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn test_registry_unregister() {
        let temp_dir = TempDir::new().unwrap();
        let registry = SandboxRegistry::with_state_dir(temp_dir.path().to_path_buf()).unwrap();

        let info = create_test_info("test-1");
        registry.register(&info).unwrap();
        assert!(registry.exists("test-1"));

        registry.unregister("test-1").unwrap();
        assert!(!registry.exists("test-1"));
    }

    #[test]
    fn test_registry_update_status() {
        let temp_dir = TempDir::new().unwrap();
        let registry = SandboxRegistry::with_state_dir(temp_dir.path().to_path_buf()).unwrap();

        let info = create_test_info("test-1");
        registry.register(&info).unwrap();

        registry
            .update_status("test-1", SandboxStatus::Running)
            .unwrap();

        let retrieved = registry.get("test-1").unwrap().unwrap();
        assert_eq!(retrieved.status, SandboxStatus::Running);
    }
}
