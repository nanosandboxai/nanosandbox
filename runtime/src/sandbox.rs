//! Sandbox management
//!
//! High-level API for creating and managing sandboxed execution environments.

use crate::config::McpServerConfig;
use crate::config::SandboxConfig;
use crate::error::{Error, Result};
use crate::image::{ImageManager, PulledImage};
use crate::oci;
use crate::oci::OciBundle;
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
    /// 1. Detect available runtime (libkrun FFI or containerd)
    /// 2. For runtimes that handle image pull (Windows containerd):
    ///    Create container/VM directly (runtime handles image internally)
    /// 3. For libkrun (Linux/macOS): Pull image, extract layers, create bundle
    pub async fn create(config: SandboxConfig) -> Result<Self> {
        let id = uuid::Uuid::new_v4().to_string();
        info!("Creating sandbox {} with image {}", id, config.image);

        // Initialize runtime to check its capabilities
        let runtime = Runtime::new().await?;
        let handles_pull = runtime.handles_image_pull();

        // Initialize image manager (needed for libkrun path)
        let image_manager = Arc::new(ImageManager::with_default_cache()?);

        let (bundle, pulled_image) = if handles_pull {
            // Runtime handles image pulling internally (Windows containerd)
            info!(
                "Using {} runtime (handles image pull internally)",
                runtime.name()
            );

            // Create the container/VM - runtime handles image pull and rootfs setup
            runtime.create(&id, &config, None).await?;

            (None, None)
        } else {
            // libkrun (Linux/macOS) - we need to pull and create bundle
            debug!("Pulling image: {}", config.image);
            let pulled = image_manager.pull(&config.image).await?;

            // Create OCI bundle directory
            let bundles_dir = image_manager.cache_dir().join("bundles");
            let bundle = OciBundle::create(&bundles_dir, &id)?;

            // Linux: merge all layers into single rootfs
            debug!("Creating rootfs from {} layers", pulled.layers.len());
            image_manager.create_rootfs(&pulled.layers, &bundle.rootfs_path)?;

            // Configure DNS in rootfs for TSI networking.
            // TSI on macOS routes guest DNS through the host, but the image's
            // resolv.conf may point to IPs blocked by the vsock IP filter.
            // Write a resolv.conf that uses the host's DNS configuration.
            Self::configure_rootfs_dns(&bundle.rootfs_path);

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
    ///
    /// Note: On Windows, this function redirects to `create()` because the Windows
    /// containerd runtime handles image pulling internally and doesn't use ImageManager.
    pub async fn create_with_manager(
        config: SandboxConfig,
        image_manager: Arc<ImageManager>,
    ) -> Result<Self> {
        // On Windows, containerd handles image pull internally - redirect to create()
        #[cfg(target_os = "windows")]
        {
            let _ = image_manager; // suppress unused warning
            return Self::create(config).await;
        }

        // Linux/macOS: use libkrun with OCI bundle creation
        #[cfg(not(target_os = "windows"))]
        {
            let id = uuid::Uuid::new_v4().to_string();
            info!("Creating sandbox {} with image {}", id, config.image);

            // Pull the image
            debug!("Pulling image: {}", config.image);
            let pulled_image = image_manager.pull(&config.image).await?;

            // Create OCI bundle directory
            let bundles_dir = image_manager.cache_dir().join("bundles");
            let bundle = OciBundle::create(&bundles_dir, &id)?;

            // Linux: merge all layers into single rootfs
            debug!(
                "Creating rootfs from {} layers",
                pulled_image.layers.len()
            );
            image_manager.create_rootfs(&pulled_image.layers, &bundle.rootfs_path)?;

            // Configure DNS in rootfs for TSI networking
            Self::configure_rootfs_dns(&bundle.rootfs_path);

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
    /// For libkrun: Creates the VM and starts it
    /// For containerd: Verifies the container exists and starts it
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
            // Runtime already exists
            rt
        } else {
            // Need to create runtime (shouldn't happen with new create flow)
            self.runtime = Some(Runtime::new().await?);
            self.runtime.as_ref().unwrap()
        };

        // For libkrun, we need to create and start the VM
        // For containerd, verify and start the container
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
            // libkrun/containerd: start just verifies the sandbox exists
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

        // Merge sandbox config env with exec-specific env (exec overrides config)
        let mut merged_env = self.config.env.clone();
        merged_env.extend(options.env);

        let start = std::time::Instant::now();

        debug!(
            "Executing command: {} {:?} (timeout: {}s, env_keys: {:?})",
            command,
            args,
            timeout_secs,
            merged_env.keys().collect::<Vec<_>>()
        );

        // Execute with timeout
        let exec_future =
            runtime.exec_with_options(&self.id, command, args, workdir, &merged_env, user);

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

        // Merge sandbox config env with exec-specific env (exec overrides config)
        let mut merged_env = self.config.env.clone();
        merged_env.extend(options.env);

        debug!(
            "Executing (streaming): {} {:?} (timeout: {}s, env_keys: {:?})",
            command,
            args,
            timeout_secs,
            merged_env.keys().collect::<Vec<_>>()
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
            &merged_env,
            user,
            callback,
        );

        let exit_code = timeout(Duration::from_secs(timeout_secs as u64), exec_future)
            .await
            .map_err(|_| Error::Timeout(timeout_secs))??;

        Ok(exit_code)
    }

    /// Send a structured agent message to the gateway (persistent mode).
    ///
    /// This is the primary API for multi-turn agent conversations. The gateway
    /// handles agent CLI spawning, session continuity (e.g., `--continue` for
    /// Claude Code), and streams output back as SSE events.
    ///
    /// Returns the exit code from the agent CLI.
    pub async fn send_message<F>(
        &self,
        message: &str,
        agent: &str,
        model: &str,
        env: &HashMap<String, String>,
        on_output: F,
    ) -> Result<i32>
    where
        F: Fn(&str, bool) + Send + Sync,
    {
        if self.status != SandboxStatus::Running {
            return Err(Error::InvalidState(format!(
                "Cannot send message in sandbox with {:?} status",
                self.status
            )));
        }

        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| Error::ExecFailed("Runtime not initialized".to_string()))?;

        // Merge sandbox config env with call-specific env
        let mut merged_env = self.config.env.clone();
        merged_env.extend(env.clone());

        let timeout_secs = self.config.timeout_secs;

        let send_future = runtime.send_message(
            &self.id,
            message,
            agent,
            model,
            &merged_env,
            |data, is_stderr| on_output(data, is_stderr),
        );

        let exit_code = timeout(Duration::from_secs(timeout_secs as u64), send_future)
            .await
            .map_err(|_| Error::Timeout(timeout_secs))??;

        Ok(exit_code)
    }

    /// Check if this sandbox is in persistent (gateway) mode.
    pub fn is_persistent(&self) -> bool {
        self.runtime
            .as_ref()
            .map(|r| r.is_persistent(&self.id))
            .unwrap_or(false)
    }

    /// Get the SSH host port for this sandbox (if available).
    pub fn ssh_port(&self) -> Option<u16> {
        self.runtime
            .as_ref()
            .and_then(|r| r.ssh_port(&self.id))
    }

    /// Get the SSH private key path for this sandbox (if available).
    pub fn ssh_key_path(&self) -> Option<std::path::PathBuf> {
        self.runtime
            .as_ref()
            .and_then(|r| r.ssh_key_path(&self.id))
    }

    /// Get a ready-to-use SSH command string for connecting to this sandbox.
    pub fn ssh_command(&self) -> Option<String> {
        self.runtime
            .as_ref()
            .and_then(|r| r.ssh_command(&self.id))
    }

    /// Add or update an MCP server in the running sandbox.
    ///
    /// Requires the sandbox to be in persistent (gateway) mode.
    /// The gateway automatically regenerates all agent config files.
    pub async fn add_mcp_server(&self, name: &str, config: McpServerConfig) -> Result<()> {
        if self.status != SandboxStatus::Running {
            return Err(Error::InvalidState(format!(
                "Cannot manage MCP servers in sandbox with {:?} status",
                self.status
            )));
        }

        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| Error::ExecFailed("Runtime not initialized".to_string()))?;

        runtime.add_mcp_server(&self.id, name, &config)
    }

    /// Remove an MCP server from the running sandbox.
    ///
    /// Requires the sandbox to be in persistent (gateway) mode.
    pub async fn remove_mcp_server(&self, name: &str) -> Result<()> {
        if self.status != SandboxStatus::Running {
            return Err(Error::InvalidState(format!(
                "Cannot manage MCP servers in sandbox with {:?} status",
                self.status
            )));
        }

        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| Error::ExecFailed("Runtime not initialized".to_string()))?;

        runtime.remove_mcp_server(&self.id, name)
    }

    /// List all MCP servers in the running sandbox.
    ///
    /// Returns the current state of all MCP servers from the gateway,
    /// including both embedded defaults and dynamically added servers.
    pub async fn list_mcp_servers(&self) -> Result<HashMap<String, McpServerConfig>> {
        if self.status != SandboxStatus::Running {
            return Err(Error::InvalidState(format!(
                "Cannot list MCP servers in sandbox with {:?} status",
                self.status
            )));
        }

        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| Error::ExecFailed("Runtime not initialized".to_string()))?;

        runtime.list_mcp_servers(&self.id)
    }

    /// Enable an MCP server in the running sandbox.
    ///
    /// Requires the sandbox to be in persistent (gateway) mode.
    pub async fn enable_mcp_server(&self, name: &str) -> Result<()> {
        if self.status != SandboxStatus::Running {
            return Err(Error::InvalidState(format!(
                "Cannot manage MCP servers in sandbox with {:?} status",
                self.status
            )));
        }

        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| Error::ExecFailed("Runtime not initialized".to_string()))?;

        runtime.enable_mcp_server(&self.id, name)
    }

    /// Disable an MCP server in the running sandbox.
    ///
    /// Requires the sandbox to be in persistent (gateway) mode.
    pub async fn disable_mcp_server(&self, name: &str) -> Result<()> {
        if self.status != SandboxStatus::Running {
            return Err(Error::InvalidState(format!(
                "Cannot manage MCP servers in sandbox with {:?} status",
                self.status
            )));
        }

        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| Error::ExecFailed("Runtime not initialized".to_string()))?;

        runtime.disable_mcp_server(&self.id, name)
    }

    /// Configure networking in the rootfs for VM networking.
    ///
    /// When gvproxy is available:
    /// - Sets DNS to gvproxy's built-in DNS at 192.168.127.1
    /// - Writes a network init script to bring up eth0 with static IP
    /// Otherwise falls back to host DNS servers.
    fn configure_rootfs_dns(rootfs_path: &std::path::Path) {
        use crate::runtime::gvproxy_available;

        let resolv_conf = rootfs_path.join("etc/resolv.conf");
        let use_gvproxy = gvproxy_available();

        let dns_servers = if use_gvproxy {
            // gvproxy provides DNS at the gateway IP
            vec!["192.168.127.1".to_string()]
        } else {
            Self::read_host_dns()
        };

        if !dns_servers.is_empty() {
            let mode = if use_gvproxy { "gvproxy" } else { "host" };
            let mut content = format!("# Generated by nanosandbox ({} networking)\n", mode);
            for dns in &dns_servers {
                content.push_str(&format!("nameserver {}\n", dns));
            }
            if let Err(e) = std::fs::write(&resolv_conf, &content) {
                warn!("Failed to write resolv.conf to rootfs: {}", e);
            } else {
                debug!("Configured rootfs DNS ({}): {:?}", mode, dns_servers);
            }
        }

        // Write network init wrapper for gvproxy.
        // This script configures eth0, then execs the user's command (passed as "$@").
        // It's used as the VM's init process (PID 1) when gvproxy networking is active.
        if use_gvproxy {
            let bin_dir = rootfs_path.join("usr/local/bin");
            let _ = std::fs::create_dir_all(&bin_dir);

            let init_script = rootfs_path.join("usr/local/bin/nanosb-net-init");
            // This script is invoked as: /bin/sh nanosb-net-init <user_cmd> <args...>
            // $0 = nanosb-net-init (or /bin/sh), $1 = user command, $2+ = args
            // shift removes the script path, so "$@" becomes the user's command.
            let script = "#!/bin/sh\n\
                if command -v ip >/dev/null 2>&1; then\n\
                    ip link set eth0 up 2>/dev/null || true\n\
                    if ! ip addr show eth0 2>/dev/null | grep -q 'inet '; then\n\
                        ip addr add 192.168.127.2/24 dev eth0 2>/dev/null || true\n\
                    fi\n\
                    if ! ip route show 2>/dev/null | grep -q 'default'; then\n\
                        ip route add default via 192.168.127.1 dev eth0 2>/dev/null || true\n\
                    fi\n\
                elif command -v ifconfig >/dev/null 2>&1; then\n\
                    ifconfig eth0 192.168.127.2 netmask 255.255.255.0 up 2>/dev/null || true\n\
                    route add default gw 192.168.127.1 2>/dev/null || true\n\
                fi\n\
                exec \"$@\"\n";
            if let Err(e) = std::fs::write(&init_script, script) {
                warn!("Failed to write network init script: {}", e);
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        &init_script,
                        std::fs::Permissions::from_mode(0o755),
                    );
                }
                debug!("Wrote gvproxy network init script to rootfs");
            }
        }
    }

    /// Read DNS servers from the host system.
    ///
    /// Reads the host's /etc/resolv.conf to get DNS servers.
    /// Falls back to well-known public DNS servers if unavailable.
    fn read_host_dns() -> Vec<String> {
        // Try /etc/resolv.conf first (works on Linux and macOS)
        if let Ok(content) = std::fs::read_to_string("/etc/resolv.conf") {
            let servers: Vec<String> = content
                .lines()
                .filter(|l| l.starts_with("nameserver"))
                .filter_map(|l| l.split_whitespace().nth(1))
                .map(|s| s.to_string())
                .collect();
            if !servers.is_empty() {
                return servers;
            }
        }

        // Fallback: common public DNS servers
        vec!["8.8.8.8".to_string(), "1.1.1.1".to_string()]
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

    #[tokio::test]
    async fn test_mcp_requires_running_state() {
        let config = SandboxConfig::builder()
            .name("test-mcp")
            .image("alpine:latest")
            .build();
        assert!(config.mcp_servers.is_empty());
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

    #[tokio::test]
    async fn test_mcp_add_requires_running_state() {
        use std::collections::HashMap;
        let sandbox = Sandbox::new_test("test-add", SandboxStatus::Ready);
        let mcp_config = McpServerConfig {
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "@upstash/context7-mcp".to_string()],
            env: HashMap::new(),
            enabled: true,
        };
        let result = sandbox.add_mcp_server("test", mcp_config).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidState(_)));
    }

    #[tokio::test]
    async fn test_mcp_remove_requires_running_state() {
        let sandbox = Sandbox::new_test("test-remove", SandboxStatus::Stopped);
        let result = sandbox.remove_mcp_server("test").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidState(_)));
    }

    #[tokio::test]
    async fn test_mcp_list_requires_running_state() {
        let sandbox = Sandbox::new_test("test-list", SandboxStatus::Creating);
        let result = sandbox.list_mcp_servers().await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidState(_)));
    }

    #[tokio::test]
    async fn test_mcp_enable_requires_running_state() {
        let sandbox = Sandbox::new_test("test-enable", SandboxStatus::Ready);
        let result = sandbox.enable_mcp_server("test").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidState(_)));
    }

    #[tokio::test]
    async fn test_mcp_disable_requires_running_state() {
        let sandbox = Sandbox::new_test("test-disable", SandboxStatus::Stopped);
        let result = sandbox.disable_mcp_server("test").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidState(_)));
    }
}
