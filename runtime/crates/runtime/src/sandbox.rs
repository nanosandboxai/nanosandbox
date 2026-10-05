//! Sandbox management
//!
//! High-level API for creating and managing sandboxed execution environments.

use crate::config::{ConsoleSpec, ExtraMount, SandboxConfig};
use crate::error::{Error, Result};
use crate::image::{ImageManager, PulledImage};
use crate::oci;
use crate::oci::OciBundle;
use crate::runtime::Runtime;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

/// Progress callback for sandbox creation phases.
///
/// Called with a human-readable status message at each step
/// (e.g. "Pulling image...", "Preparing rootfs...", "Configuring network...").
pub type ProgressFn = dyn Fn(&str) + Send + Sync;

/// Sandbox lifecycle status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxStatus {
    /// Sandbox is being created
    #[default]
    Creating,
    /// Sandbox is ready but not started
    Ready,
    /// Sandbox is running
    Running,
    /// Sandbox is stopped
    Stopped,
    /// Sandbox encountered an error
    Error,
}

/// A managed sandbox instance
///
/// The runtime Sandbox is a pure VM engine: it creates, boots, stops, and
/// destroys microVMs. Gateway communication, SSH, exec, and secrets are
/// handled by the `gateway` crate in the sandbox layer.
#[allow(dead_code)]
pub struct Sandbox {
    /// Unique identifier
    id: String,
    /// Configuration
    config: SandboxConfig,
    /// Current status
    status: SandboxStatus,
    /// Runtime instance
    runtime: Option<Runtime>,
    /// OCI bundle
    bundle: Option<OciBundle>,
    /// Image manager
    image_manager: Arc<ImageManager>,
    /// Pulled image info
    pulled_image: Option<PulledImage>,
    /// Creation timestamp
    created_at: DateTime<Utc>,
}

impl Sandbox {
    /// Create a new sandbox from configuration
    ///
    /// This will:
    /// 1. Detect available runtime (libkrun FFI on all platforms)
    /// 2. Pull image, extract layers, create OCI bundle
    /// 3. Create the VM via libkrun
    pub async fn create(config: SandboxConfig) -> Result<Self> {
        Self::create_with_progress(config, None).await
    }

    /// Create a new sandbox with a progress callback for status updates.
    ///
    /// The callback is invoked at each phase of sandbox creation with a
    /// human-readable message (e.g. "Pulling image...", "Preparing rootfs...").
    pub async fn create_with_progress(
        config: SandboxConfig,
        on_progress: Option<&ProgressFn>,
    ) -> Result<Self> {
        let progress = |msg: &str| {
            if let Some(cb) = on_progress {
                cb(msg);
            }
        };

        let id = uuid::Uuid::new_v4().to_string();
        info!("Creating sandbox {} with image {}", id, config.image);

        // Initialize runtime
        let runtime = Runtime::new().await?;

        // Initialize image manager
        let image_manager = Arc::new(ImageManager::with_default_cache()?);

        // Pull image and create OCI bundle (all platforms use libkrun)
        progress("Pulling image...");
        debug!("Pulling image: {}", config.image);
        let pulled = image_manager.pull(&config.image).await?;

        // Create OCI bundle directory (empty rootfs)
        let bundles_dir = image_manager.cache_dir().join("bundles");
        let bundle = OciBundle::create(&bundles_dir, &id)?;

        // Extract all layers into the bundle's rootfs. The manifest
        // config_digest enables the golden-rootfs cache: subsequent
        // sandboxes for the same image clone the cached tree via
        // APFS clonefile (macOS) or recursive copy (Linux).
        progress("Preparing rootfs...");
        debug!("Creating rootfs from {} layers", pulled.layers.len());
        image_manager.create_rootfs(
            &pulled.layers,
            &bundle.rootfs_path,
            Some(&pulled.config_digest),
        )?;

        progress("Generating config...");
        let oci_config = oci::generate_config(&config, &bundle.rootfs_path);
        bundle.write_config(&oci_config)?;

        let (bundle, pulled_image) = (Some(bundle), Some(pulled));

        info!("Sandbox {} created successfully", id);

        Ok(Self {
            id,
            config,
            status: SandboxStatus::Ready,
            runtime: Some(runtime),
            bundle,
            image_manager,
            pulled_image,
            created_at: Utc::now(),
        })
    }

    /// Create a sandbox with a pre-existing image manager
    pub async fn create_with_manager(
        config: SandboxConfig,
        image_manager: Arc<ImageManager>,
    ) -> Result<Self> {
        Self::create_with_manager_progress(config, image_manager, None).await
    }

    /// Create a sandbox with a pre-existing image manager and progress callback.
    pub async fn create_with_manager_progress(
        config: SandboxConfig,
        image_manager: Arc<ImageManager>,
        on_progress: Option<&ProgressFn>,
    ) -> Result<Self> {
        let progress = |msg: &str| {
            if let Some(cb) = on_progress {
                cb(msg);
            }
        };

        {
            let id = uuid::Uuid::new_v4().to_string();
            info!("Creating sandbox {} with image {}", id, config.image);

            // Pull the image
            progress("Pulling image...");
            debug!("Pulling image: {}", config.image);
            let pulled_image = image_manager.pull(&config.image).await?;

            // Create OCI bundle directory (empty rootfs)
            let bundles_dir = image_manager.cache_dir().join("bundles");
            let bundle = OciBundle::create(&bundles_dir, &id)?;

            // Extract all layers into the bundle's rootfs, using the golden
            // rootfs cache (keyed on config_digest) to avoid re-extraction.
            progress("Preparing rootfs...");
            debug!("Creating rootfs from {} layers", pulled_image.layers.len());
            image_manager.create_rootfs(
                &pulled_image.layers,
                &bundle.rootfs_path,
                Some(&pulled_image.config_digest),
            )?;

            progress("Generating config...");
            let oci_config = oci::generate_config(&config, &bundle.rootfs_path);
            bundle.write_config(&oci_config)?;

            info!("Sandbox {} created successfully", id);

            Ok(Self {
                id,
                config,
                status: SandboxStatus::Ready,
                runtime: None,
                bundle: Some(bundle),
                image_manager,
                pulled_image: Some(pulled_image),
                created_at: Utc::now(),
            })
        }
    }

    /// Get the sandbox ID
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Get the sandbox configuration
    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }

    /// Get a mutable reference to the configuration.
    /// Used by the sandbox layer to set command/gateway before start().
    pub fn config_mut(&mut self) -> &mut SandboxConfig {
        &mut self.config
    }

    /// Get the current status
    pub fn status(&self) -> SandboxStatus {
        self.status
    }

    /// Get the bundle path
    pub fn bundle_path(&self) -> Option<&PathBuf> {
        self.bundle.as_ref().map(|b| &b.path)
    }

    /// Get a reference to the runtime instance (if started).
    pub fn runtime_ref(&self) -> Option<&Runtime> {
        self.runtime.as_ref()
    }

    /// Get the creation timestamp
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// Start the sandbox in next mode with console fds and extra mounts.
    ///
    /// This is the zero-image-customization path. The supervisor provides
    /// console fds and extra mount specs. The VM subprocess inherits the fds.
    pub async fn start_next(
        &mut self,
        console: Option<ConsoleSpec>,
        extra_mounts: Vec<ExtraMount>,
    ) -> Result<()> {
        if self.status != SandboxStatus::Ready && self.status != SandboxStatus::Stopped {
            error!(sandbox_id = %self.id, "Cannot start sandbox in {:?} state", self.status);
            return Err(Error::InvalidState(format!(
                "Cannot start sandbox in {:?} state",
                self.status
            )));
        }

        info!("Starting sandbox {} (next mode)", self.id);

        let runtime = if let Some(ref rt) = self.runtime {
            rt
        } else {
            self.runtime = Some(Runtime::new().await?);
            self.runtime.as_ref().expect("runtime was just assigned")
        };

        let bundle = self
            .bundle
            .as_ref()
            .ok_or_else(|| {
                error!(sandbox_id = %self.id, "No bundle available for sandbox start");
                Error::SandboxCreationFailed("No bundle available".to_string())
            })?;

        let create_timeout = Duration::from_secs(300);
        let start_timeout = Duration::from_secs(600);

        debug!("Creating VM via runtime");
        timeout(
            create_timeout,
            runtime.create(&self.id, &self.config, Some(&bundle.path)),
        )
        .await
        .map_err(|_| {
            error!(sandbox_id = %self.id, "Sandbox VM creation timed out after 300s");
            Error::Timeout(300)
        })?
        .inspect_err(|_| {
            self.status = SandboxStatus::Error;
        })?;

        debug!("Starting VM (next mode) via runtime");
        timeout(start_timeout, runtime.start_next(&self.id, console, extra_mounts))
            .await
            .map_err(|_| {
                error!(sandbox_id = %self.id, "Sandbox VM start timed out after 600s");
                Error::Timeout(600)
            })?
            .inspect_err(|_| {
                self.status = SandboxStatus::Error;
            })?;

        self.status = SandboxStatus::Running;
        info!("Sandbox {} is now running (next mode)", self.id);
        Ok(())
    }

    /// Start the sandbox
    ///
    /// Creates and boots the VM via libkrun. Gateway communication, SSH
    /// setup, and health checks are handled by the sandbox layer above.
    pub async fn start(&mut self) -> Result<()> {
        if self.status != SandboxStatus::Ready && self.status != SandboxStatus::Stopped {
            error!(sandbox_id = %self.id, "Cannot start sandbox in {:?} state", self.status);
            return Err(Error::InvalidState(format!(
                "Cannot start sandbox in {:?} state",
                self.status
            )));
        }

        info!("Starting sandbox {}", self.id);

        // Get or create runtime
        let runtime = if let Some(ref rt) = self.runtime {
            rt
        } else {
            self.runtime = Some(Runtime::new().await?);
            self.runtime.as_ref().expect("runtime was just assigned")
        };

        let bundle = self
            .bundle
            .as_ref()
            .ok_or_else(|| {
                error!(sandbox_id = %self.id, "No bundle available for sandbox start");
                Error::SandboxCreationFailed("No bundle available".to_string())
            })?;

        // --- Create and start the VM ---

        // Timeout must accommodate first-run rootfs creation
        // plus VM boot.
        let create_timeout = Duration::from_secs(300);
        let start_timeout = Duration::from_secs(600);

        debug!("Creating VM via runtime");
        timeout(
            create_timeout,
            runtime.create(&self.id, &self.config, Some(&bundle.path)),
        )
        .await
        .map_err(|_| {
            error!(sandbox_id = %self.id, "Sandbox VM creation timed out after 300s");
            Error::Timeout(300)
        })?
        .inspect_err(|_| {
            self.status = SandboxStatus::Error;
        })?;

        debug!("Starting VM via runtime");
        timeout(start_timeout, runtime.start(&self.id))
            .await
            .map_err(|_| {
                error!(sandbox_id = %self.id, "Sandbox VM start timed out after 600s");
                Error::Timeout(600)
            })?
            .inspect_err(|_| {
                self.status = SandboxStatus::Error;
            })?;

        self.status = SandboxStatus::Running;
        info!("Sandbox {} is now running", self.id);
        Ok(())
    }

    /// Dynamically forward a guest port to the same host port via gvproxy.
    pub fn expose_port(&self, port: u16) -> std::result::Result<(), String> {
        self.runtime
            .as_ref()
            .ok_or_else(|| "no runtime".to_string())
            .and_then(|r| r.expose_port(&self.id, port))
    }

    /// Get the guest IP address for this sandbox.
    pub fn guest_ip(&self) -> Option<String> {
        self.runtime
            .as_ref()
            .and_then(|r| r.guest_ip(&self.id))
    }

    /// Stop the sandbox
    pub async fn stop(&mut self) -> Result<()> {
        if self.status != SandboxStatus::Running {
            return Ok(()); // Already stopped
        }

        info!("Stopping sandbox {}", self.id);

        if let Some(ref runtime) = self.runtime {
            runtime.kill(&self.id).await?;
        }

        self.status = SandboxStatus::Stopped;
        info!("Sandbox {} stopped", self.id);
        Ok(())
    }

    /// Restart a stopped sandbox
    pub async fn restart(&mut self) -> Result<()> {
        if self.status == SandboxStatus::Running {
            self.stop().await?;
        }

        if self.status != SandboxStatus::Stopped {
            error!(sandbox_id = %self.id, "Cannot restart sandbox in {:?} state", self.status);
            return Err(Error::InvalidState(format!(
                "Cannot restart sandbox in {:?} state",
                self.status
            )));
        }

        let bundle = self
            .bundle
            .as_ref()
            .ok_or_else(|| {
                error!(sandbox_id = %self.id, "No bundle available for sandbox restart");
                Error::SandboxCreationFailed("No bundle available".to_string())
            })?;

        info!("Restarting sandbox {}", self.id);

        // Initialize runtime
        let runtime = Runtime::new().await?;

        // Delete old container state if exists
        runtime.delete(&self.id).await?;

        // Create and start the container again
        debug!("Creating container via runtime");
        runtime
            .create(&self.id, &self.config, Some(&bundle.path))
            .await?;

        debug!("Starting container via runtime");
        runtime.start(&self.id).await?;

        self.runtime = Some(runtime);
        self.status = SandboxStatus::Running;

        info!("Sandbox {} restarted successfully", self.id);
        Ok(())
    }

    /// Destroy the sandbox and clean up all resources
    pub async fn destroy(mut self) -> Result<()> {
        info!("Destroying sandbox {}", self.id);

        // Stop if running
        if self.status == SandboxStatus::Running {
            if let Err(e) = self.stop().await {
                warn!("Failed to stop sandbox {} during destroy: {}", self.id, e);
            }
        }

        // Delete from runtime
        if let Some(ref runtime) = self.runtime {
            if let Err(e) = runtime.delete(&self.id).await {
                warn!("Failed to delete sandbox {} from runtime: {}", self.id, e);
            }
        }

        // Remove the bundle directory.
        if let Some(bundle) = &self.bundle {
            if bundle.path.exists() {
                if let Err(e) = std::fs::remove_dir_all(&bundle.path) {
                    warn!("Failed to remove bundle directory: {}", e);
                }
            }
        }

        info!("Sandbox {} destroyed", self.id);
        Ok(())
    }

    /// Create a sandbox in a specific state for testing.
    #[cfg(test)]
    fn new_test(id: &str, status: SandboxStatus) -> Self {
        use std::sync::Arc;
        let cache_dir = std::env::temp_dir().join(format!("nanosandbox-test-{}", id));
        let _ = std::fs::create_dir_all(&cache_dir);
        let image_manager = Arc::new(ImageManager::new(cache_dir).expect("test image manager"));
        Sandbox {
            id: id.to_string(),
            config: SandboxConfig::builder()
                .name("test")
                .image("alpine:latest")
                .build(),
            status,
            runtime: None,
            bundle: None,
            image_manager,
            pulled_image: None,
            created_at: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_sandbox_status() {
        assert_eq!(SandboxStatus::default(), SandboxStatus::Creating);
    }

    #[test]
    fn test_sandbox_config_with_project_field() {
        let config = SandboxConfig::builder()
            .name("test-project")
            .image("alpine")
            .project("/tmp/test", None)
            .build();
        assert!(config.project.is_some());
    }

}
