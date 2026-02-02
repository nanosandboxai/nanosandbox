//! Sandbox management
//!
//! High-level API for creating and managing sandboxed execution environments.

use crate::config::SandboxConfig;
use crate::error::{Error, Result};
use crate::image::{ImageManager, PulledImage};
use crate::oci::{self, OciBundle};
use crate::runtime::Runtime;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use tracing::{debug, info, warn};

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

/// Result of command execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    /// Exit code
    pub exit_code: i32,
    /// Standard output
    pub stdout: String,
    /// Standard error
    pub stderr: String,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
}

impl ExecResult {
    /// Check if the command succeeded
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Output stream type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// Standard output
    Stdout,
    /// Standard error
    Stderr,
}

/// Chunk of streaming output
#[derive(Debug, Clone)]
pub struct OutputChunk {
    /// The stream this chunk came from
    pub stream: Stream,
    /// The data
    pub data: String,
    /// Timestamp
    pub timestamp: DateTime<Utc>,
}

/// Options for command execution
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    /// Working directory (defaults to sandbox workdir)
    pub workdir: Option<String>,
    /// Additional environment variables
    pub env: HashMap<String, String>,
    /// User to run as (defaults to root)
    pub user: Option<String>,
    /// Timeout override in seconds (uses sandbox default if None)
    pub timeout_secs: Option<u32>,
}

impl ExecOptions {
    /// Create new exec options
    pub fn new() -> Self {
        Self::default()
    }

    /// Set working directory
    pub fn workdir(mut self, dir: impl Into<String>) -> Self {
        self.workdir = Some(dir.into());
        self
    }

    /// Add an environment variable
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Set user to run as
    pub fn user(mut self, user: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self
    }

    /// Set timeout in seconds
    pub fn timeout(mut self, secs: u32) -> Self {
        self.timeout_secs = Some(secs);
        self
    }
}

/// A managed sandbox instance
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
    /// 1. Detect available runtime (OCI or krunvm)
    /// 2. For OCI runtimes: Pull image, extract layers, create bundle
    /// 3. For krunvm: Create VM directly (handles image internally)
    pub async fn create(config: SandboxConfig) -> Result<Self> {
        let id = uuid::Uuid::new_v4().to_string();
        info!("Creating sandbox {} with image {}", id, config.image);

        // Initialize runtime to check its capabilities
        let runtime = Runtime::new().await?;
        let handles_pull = runtime.handles_image_pull();

        // Initialize image manager
        let image_manager = Arc::new(ImageManager::with_default_cache()?);

        let (bundle, pulled_image) = if handles_pull {
            // krunvm handles image pulling internally
            info!(
                "Using {} runtime (handles image pull internally)",
                runtime.name()
            );

            // Create the VM via krunvm (no bundle needed)
            runtime.create(&id, &config, None).await?;

            (None, None)
        } else {
            // OCI runtime - we need to pull and create bundle
            debug!("Pulling image: {}", config.image);
            let pulled = image_manager.pull(&config.image).await?;

            // Create OCI bundle directory
            let bundles_dir = image_manager.cache_dir().join("bundles");
            let bundle = OciBundle::create(&bundles_dir, &id)?;

            // Extract layers to create rootfs
            debug!("Creating rootfs from {} layers", pulled.layers.len());
            image_manager.create_rootfs(&pulled.layers, &bundle.rootfs_path)?;

            // Generate and write OCI config
            let oci_config = oci::generate_config(&config, &bundle.rootfs_path);
            bundle.write_config(&oci_config)?;

            (Some(bundle), Some(pulled))
        };

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
        let id = uuid::Uuid::new_v4().to_string();
        info!("Creating sandbox {} with image {}", id, config.image);

        // Pull the image
        debug!("Pulling image: {}", config.image);
        let pulled_image = image_manager.pull(&config.image).await?;

        // Create OCI bundle directory
        let bundles_dir = image_manager.cache_dir().join("bundles");
        let bundle = OciBundle::create(&bundles_dir, &id)?;

        // Extract layers to create rootfs
        debug!("Creating rootfs from {} layers", pulled_image.layers.len());
        image_manager.create_rootfs(&pulled_image.layers, &bundle.rootfs_path)?;

        // Generate and write OCI config
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

    /// Get the sandbox ID
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Get the sandbox configuration
    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }

    /// Get the current status
    pub fn status(&self) -> SandboxStatus {
        self.status
    }

    /// Get the bundle path
    pub fn bundle_path(&self) -> Option<&PathBuf> {
        self.bundle.as_ref().map(|b| &b.path)
    }

    /// Get the creation timestamp
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// Start the sandbox
    ///
    /// For OCI runtimes: Creates and starts the container
    /// For krunvm: VM is created during `Sandbox::create`, this just marks it ready
    pub async fn start(&mut self) -> Result<()> {
        if self.status != SandboxStatus::Ready && self.status != SandboxStatus::Stopped {
            return Err(Error::InvalidState(format!(
                "Cannot start sandbox in {:?} state",
                self.status
            )));
        }

        info!("Starting sandbox {}", self.id);

        // Get or create runtime
        let runtime = if let Some(ref rt) = self.runtime {
            // Runtime already exists (krunvm case)
            rt
        } else {
            // Need to create runtime (shouldn't happen with new create flow)
            self.runtime = Some(Runtime::new().await?);
            self.runtime.as_ref().unwrap()
        };

        // For OCI runtimes, we need to create and start the container
        // For krunvm, the VM was created in Sandbox::create
        if !runtime.handles_image_pull() {
            let bundle = self
                .bundle
                .as_ref()
                .ok_or_else(|| Error::SandboxCreationFailed("No bundle available".to_string()))?;

            let start_timeout = Duration::from_secs(60);

            // Create the container
            debug!("Creating container via runtime");
            timeout(
                start_timeout,
                runtime.create(&self.id, &self.config, Some(&bundle.path)),
            )
            .await
            .map_err(|_| Error::Timeout(60))?
            .inspect_err(|_| {
                self.status = SandboxStatus::Error;
            })?;

            // Start the container
            debug!("Starting container via runtime");
            timeout(start_timeout, runtime.start(&self.id))
                .await
                .map_err(|_| Error::Timeout(60))?
                .inspect_err(|_| {
                    self.status = SandboxStatus::Error;
                })?;
        } else {
            // krunvm: start just verifies the VM exists
            runtime.start(&self.id).await?;
        }

        self.status = SandboxStatus::Running;
        info!("Sandbox {} is now running", self.id);
        Ok(())
    }

    /// Execute a command in the sandbox
    pub async fn exec(&self, command: &str, args: &[&str]) -> Result<ExecResult> {
        self.exec_with_options(command, args, ExecOptions::default())
            .await
    }

    /// Execute a command with options
    pub async fn exec_with_options(
        &self,
        command: &str,
        args: &[&str],
        options: ExecOptions,
    ) -> Result<ExecResult> {
        if self.status != SandboxStatus::Running {
            return Err(Error::InvalidState(format!(
                "Cannot exec in sandbox with {:?} status",
                self.status
            )));
        }

        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| Error::ExecFailed("Runtime not initialized".to_string()))?;

        let timeout_secs = options.timeout_secs.unwrap_or(self.config.timeout_secs);
        let workdir = options.workdir.as_deref();
        let user = options.user.as_deref();

        let start = std::time::Instant::now();

        debug!(
            "Executing command: {} {:?} (timeout: {}s)",
            command, args, timeout_secs
        );

        // Execute with timeout
        let exec_future =
            runtime.exec_with_options(&self.id, command, args, workdir, &options.env, user);

        let output = timeout(Duration::from_secs(timeout_secs as u64), exec_future)
            .await
            .map_err(|_| Error::Timeout(timeout_secs))??;

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(ExecResult {
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
            duration_ms,
        })
    }

    /// Execute a command with streaming output
    pub async fn exec_stream<F>(&self, command: &str, args: &[&str], on_output: F) -> Result<i32>
    where
        F: Fn(OutputChunk) + Send + Sync,
    {
        self.exec_stream_with_options(command, args, ExecOptions::default(), on_output)
            .await
    }

    /// Execute a command with streaming output and options
    pub async fn exec_stream_with_options<F>(
        &self,
        command: &str,
        args: &[&str],
        options: ExecOptions,
        on_output: F,
    ) -> Result<i32>
    where
        F: Fn(OutputChunk) + Send + Sync,
    {
        if self.status != SandboxStatus::Running {
            return Err(Error::InvalidState(format!(
                "Cannot exec in sandbox with {:?} status",
                self.status
            )));
        }

        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| Error::ExecFailed("Runtime not initialized".to_string()))?;

        let timeout_secs = options.timeout_secs.unwrap_or(self.config.timeout_secs);
        let workdir = options.workdir.as_deref();
        let user = options.user.as_deref();

        debug!(
            "Executing (streaming): {} {:?} (timeout: {}s)",
            command, args, timeout_secs
        );

        // Wrapper to convert runtime callback to our OutputChunk format
        let callback = |data: &str, is_stderr: bool| {
            on_output(OutputChunk {
                stream: if is_stderr {
                    Stream::Stderr
                } else {
                    Stream::Stdout
                },
                data: data.to_string(),
                timestamp: Utc::now(),
            });
        };

        // Execute with timeout
        let exec_future = runtime.exec_stream(
            &self.id,
            command,
            args,
            workdir,
            &options.env,
            user,
            callback,
        );

        let exit_code = timeout(Duration::from_secs(timeout_secs as u64), exec_future)
            .await
            .map_err(|_| Error::Timeout(timeout_secs))??;

        Ok(exit_code)
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
            return Err(Error::InvalidState(format!(
                "Cannot restart sandbox in {:?} state",
                self.status
            )));
        }

        let bundle = self
            .bundle
            .as_ref()
            .ok_or_else(|| Error::SandboxCreationFailed("No bundle available".to_string()))?;

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
            self.stop().await?;
        }

        // Delete from runtime
        if let Some(ref runtime) = self.runtime {
            runtime.delete(&self.id).await?;
        }

        // Remove the bundle directory
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_sandbox_status() {
        assert_eq!(SandboxStatus::default(), SandboxStatus::Creating);
    }

    #[test]
    fn test_exec_result_success() {
        let result = ExecResult {
            exit_code: 0,
            stdout: "hello".to_string(),
            stderr: String::new(),
            duration_ms: 100,
        };
        assert!(result.success());

        let result_fail = ExecResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: "error".to_string(),
            duration_ms: 50,
        };
        assert!(!result_fail.success());
    }

    #[test]
    fn test_exec_options_builder() {
        let options = ExecOptions::new()
            .workdir("/app")
            .env("FOO", "bar")
            .user("nobody")
            .timeout(30);

        assert_eq!(options.workdir, Some("/app".to_string()));
        assert_eq!(options.env.get("FOO"), Some(&"bar".to_string()));
        assert_eq!(options.user, Some("nobody".to_string()));
        assert_eq!(options.timeout_secs, Some(30));
    }
}
