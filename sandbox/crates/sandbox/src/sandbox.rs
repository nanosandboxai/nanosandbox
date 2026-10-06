//! Sandbox wrapper that composes runtime::Sandbox with project-mount management.
//!
//! The `runtime` crate provides a pure microVM runtime with no knowledge of
//! project mounts, gateway helpers, or agent sessions. This `Sandbox` wraps
//! `runtime::Sandbox` and adds the sandbox-layer concerns, starting with
//! project-mount lifecycle (git clone + auto-commit on teardown).

use crate::project::{BranchStrategy, ProjectMount};
use runtime::{ProgressFn, Result, RuntimeMode, SandboxConfig};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info, warn};

/// Agent-layer sandbox: a `runtime::Sandbox` plus a project mount and gateway client.
///
/// Derefs to `runtime::Sandbox` so all VM lifecycle methods (`start`, `stop`,
/// `exec`, etc.) are available directly. `create` / `create_with_manager` set
/// up the project mount before VM creation; `destroy` tears it down after.
pub struct Sandbox {
    inner: runtime::Sandbox,
    project_mount: Option<ProjectMount>,
    gateway: Option<gateway::GatewayClient>,
    ssh_port: Option<u16>,
    ssh_host: Option<String>,
    ssh_key_path: Option<PathBuf>,
}

impl Sandbox {
    /// Create a new sandbox from configuration.
    ///
    /// If `config.project` is set, a `ProjectMount` is detected and set up
    /// before the VM is created; the resulting virtiofs mount is added to
    /// `config.mounts` so it appears in the OCI config.
    pub async fn create(config: SandboxConfig) -> Result<Self> {
        Self::create_with_progress(config, None).await
    }

    /// Create a new sandbox with a progress callback.
    pub async fn create_with_progress(
        mut config: SandboxConfig,
        on_progress: Option<&ProgressFn>,
    ) -> Result<Self> {
        let project_mount = setup_project_mount(&mut config, on_progress)?;
        let inner = runtime::Sandbox::create_with_progress(config, on_progress).await?;
        Ok(Self {
            inner,
            project_mount,
            gateway: None,
            ssh_port: None,
            ssh_host: None,
            ssh_key_path: None,
        })
    }

    /// Create a sandbox with a pre-existing image manager.
    pub async fn create_with_manager(
        config: SandboxConfig,
        image_manager: Arc<runtime::ImageManager>,
    ) -> Result<Self> {
        Self::create_with_manager_progress(config, image_manager, None).await
    }

    /// Create a sandbox with a pre-existing image manager and progress callback.
    pub async fn create_with_manager_progress(
        mut config: SandboxConfig,
        image_manager: Arc<runtime::ImageManager>,
        on_progress: Option<&ProgressFn>,
    ) -> Result<Self> {
        let project_mount = setup_project_mount(&mut config, on_progress)?;
        let inner = runtime::Sandbox::create_with_manager_progress(
            config,
            image_manager,
            on_progress,
        )
        .await?;
        Ok(Self {
            inner,
            project_mount,
            gateway: None,
            ssh_port: None,
            ssh_host: None,
            ssh_key_path: None,
        })
    }

    /// Wrap an already-created runtime::Sandbox without a project mount.
    ///
    /// Useful when the caller manages the project mount externally (e.g. the
    /// CLI's TUI resume flow, where the mount lives on a panel).
    pub fn from_runtime(inner: runtime::Sandbox) -> Self {
        Self {
            inner,
            project_mount: None,
            gateway: None,
            ssh_port: None,
            ssh_host: None,
            ssh_key_path: None,
        }
    }

    /// Get a reference to the inner runtime sandbox.
    pub fn runtime_sandbox(&self) -> &runtime::Sandbox {
        &self.inner
    }

    /// Id of the underlying runtime sandbox.
    pub fn id(&self) -> &str {
        self.inner.id()
    }

    /// Host-side exec client for this sandbox's vsock exec channel.
    pub fn exec_client(&self) -> runtime::exec::ExecClient {
        let cfg = self.inner.config();
        match cfg.vsock_socket.clone() {
            Some(sock) => runtime::exec::ExecClient::new(sock),
            None => runtime::exec::ExecClient::new(String::new()),
        }
    }

    /// True when this sandbox runs in next (console/vsock) mode.
    pub fn is_next_mode(&self) -> bool {
        self.inner.config().runtime_mode == RuntimeMode::Next
    }

    /// Get a reference to the active project mount, if any.
    pub fn project_mount(&self) -> Option<&ProjectMount> {
        self.project_mount.as_ref()
    }

    /// Get a mutable reference to the active project mount, if any.
    pub fn project_mount_mut(&mut self) -> Option<&mut ProjectMount> {
        self.project_mount.as_mut()
    }

    /// Take ownership of the project mount (for transferring to TUI panels).
    pub fn take_project_mount(&mut self) -> Option<ProjectMount> {
        self.project_mount.take()
    }

    // -- Gateway accessors --

    /// Get a reference to the gateway client, or error if not available.
    pub fn gateway(&self) -> gateway::Result<&gateway::GatewayClient> {
        self.gateway
            .as_ref()
            .ok_or(gateway::Error::NotAvailable)
    }

    /// Get a mutable reference to the gateway client, or error if not available.
    pub fn gateway_mut(&mut self) -> gateway::Result<&mut gateway::GatewayClient> {
        self.gateway
            .as_mut()
            .ok_or(gateway::Error::NotAvailable)
    }

    /// SSH port for connecting to the sandbox, if available.
    pub fn ssh_port(&self) -> Option<u16> {
        self.ssh_port
    }

    /// SSH host for connecting to the sandbox.
    pub fn ssh_host(&self) -> Option<String> {
        self.ssh_host.clone()
    }

    /// Path to the SSH private key for this sandbox, if available.
    pub fn ssh_key_path(&self) -> Option<PathBuf> {
        self.ssh_key_path.clone()
    }

    /// Build an SSH command string for connecting to the sandbox.
    pub fn ssh_command(&self) -> Option<String> {
        let port = self.ssh_port?;
        let key_path = self.ssh_key_path.as_ref()?;
        Some(format!(
            "ssh -i {} -p {} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null root@127.0.0.1",
            key_path.display(),
            port
        ))
    }

    /// Start the sandbox.
    ///
    /// Before booting the VM, detects gateway mode from the rootfs, sets up
    /// port mappings and SSH keys, boots the VM, creates a `GatewayClient`,
    /// performs health checks, and generates the secrets keypair.
    ///
    /// In "next" runtime mode, gateway detection and SSH setup are skipped
    /// entirely — the VM boots a vanilla image with console I/O.
    pub async fn start(&mut self) -> Result<()> {
        // In next mode, skip gateway detection/SSH/port-mapping entirely.
        if self.inner.config().runtime_mode == RuntimeMode::Next {
            info!(
                "Next runtime mode: skipping gateway setup for sandbox '{}'",
                self.inner.id()
            );
            self.inner.start().await?;
            return Ok(());
        }

        // Detect gateway mode from the bundle rootfs.
        let has_gateway = if let Some(bundle_path) = self.inner.bundle_path() {
            let rootfs = bundle_path.join("rootfs");
            let has_init = rootfs.join("usr/local/bin/nanosb-init.sh").exists()
                || rootfs.join("usr/local/bin/agent-gateway").exists()
                || rootfs.join(".nanosb-layers").exists();

            if has_init {
                self.inner.config_mut().command =
                    Some("/usr/local/bin/nanosb-init.sh".to_string());
                self.inner.config_mut().command_args = vec![];
            }

            has_init
        } else {
            false
        };

        if has_gateway {
            info!(
                "Gateway mode enabled for sandbox '{}' — persistent VM mode",
                self.inner.id()
            );

            // Ensure port mappings for gateway (8080) and SSH (22).
            // Host ports are allocated dynamically to avoid conflicts.
            use runtime::PortMapping;
            let needs_8080 = !self
                .inner
                .config_mut()
                .network
                .port_mappings
                .iter()
                .any(|p| p.container_port == 8080);
            let needs_22 = !self
                .inner
                .config_mut()
                .network
                .port_mappings
                .iter()
                .any(|p| p.container_port == 22);

            if needs_8080 {
                let host_port = allocate_ephemeral_port().unwrap_or(8080);
                info!("Gateway port mapping: host:{} -> guest:8080", host_port);
                self.inner
                    .config_mut()
                    .network
                    .port_mappings
                    .push(PortMapping {
                        host_port,
                        container_port: 8080,
                        protocol: "tcp".to_string(),
                    });
            }
            if needs_22 {
                let host_port = allocate_ephemeral_port().unwrap_or(2222);
                info!("SSH port mapping: host:{} -> guest:22", host_port);
                self.inner
                    .config_mut()
                    .network
                    .port_mappings
                    .push(PortMapping {
                        host_port,
                        container_port: 22,
                        protocol: "tcp".to_string(),
                    });
            }

            // Generate SSH keys and inject pubkey into rootfs.
            if let Some(bundle_path) = self.inner.bundle_path() {
                let rootfs_path = bundle_path.join("rootfs");
                match gateway::generate_ssh_keys(self.inner.id()) {
                    Ok((key_path, pubkey)) => {
                        info!(
                            "SSH keys generated for sandbox '{}': {}",
                            self.inner.id(),
                            key_path.display()
                        );
                        if let Err(e) =
                            gateway::inject_pubkey_into_rootfs(&rootfs_path, &pubkey)
                        {
                            warn!(
                                "Failed to inject SSH pubkey into rootfs: {} (SSH may not work)",
                                e
                            );
                        }
                        self.inner.config_mut().ssh_pubkey =
                            Some(pubkey.trim().to_string());
                        self.ssh_key_path = Some(key_path);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to generate SSH keys for sandbox '{}': {} (SSH access unavailable)",
                            self.inner.id(),
                            e
                        );
                    }
                }
            }
        }

        // Boot the VM via the runtime.
        self.inner.start().await?;

        // Post-boot: set up gateway client and health checks.
        if has_gateway {
            let sandbox_id = self.inner.id().to_string();
            let config_env = self.inner.config_mut().env.clone();

            // Determine gateway address and SSH port from port mappings.
            let mut gateway_addr: Option<String> = None;

            let port_mappings = &self.inner.config().network.port_mappings;
            let find_host_port = |container: u16| {
                port_mappings
                    .iter()
                    .find(|p| p.container_port == container)
                    .map(|p| p.host_port)
            };

            if let Some(host_port) = find_host_port(8080) {
                gateway_addr = Some(format!("127.0.0.1:{}", host_port));
            }
            if let Some(host_port) = find_host_port(22) {
                self.ssh_port = Some(host_port);
            }

            // Fallback to guest_ip for non-gvproxy backends (e.g. TSI).
            if gateway_addr.is_none() {
                if let Some(ref rt) = self.inner.runtime_ref() {
                    if let Some(ip) = rt.guest_ip(self.inner.id()) {
                        gateway_addr = Some(format!("{}:8080", ip));
                        if self.ssh_port.is_none() {
                            self.ssh_port = Some(22);
                        }
                        info!("Gateway: {}:8080 (guest_ip path)", ip);
                    }
                }
            }

            if let Some(ref addr) = gateway_addr {
                info!("Gateway address: {}", addr);
            }
            if let Some(ssh) = self.ssh_port {
                info!("SSH port: {}", ssh);
            }

            // Create the GatewayClient.
            let mut client = gateway::GatewayClient::new(
                gateway_addr,
                sandbox_id.clone(),
                300, // default timeout
                config_env,
            );

            // Wait for the gateway to become healthy.
            // The closure checks if the VM process is still alive via the runtime.
            let sandbox_id = sandbox_id.clone();
            let runtime_ref = self.inner.runtime_ref();
            client
                .wait_for_health(|| {
                    runtime_ref
                        .map(|rt| rt.is_vm_running(&sandbox_id))
                        .unwrap_or(false)
                })
                .await
                .map_err(|e| {
                    runtime::Error::SandboxCreationFailed(format!(
                        "Gateway health check failed: {}",
                        e
                    ))
                })?;

            // Generate the secrets keypair.
            client.generate_secrets_keypair();

            self.gateway = Some(client);
        }

        Ok(())
    }

    /// Stop the sandbox gracefully.
    pub async fn stop(&mut self) -> Result<()> {
        // Gracefully stop the gateway before killing the VM.
        if let Some(ref gw) = self.gateway {
            if let Err(e) = gw.stop().await {
                warn!("Failed to gracefully stop gateway: {}", e);
            }
        }
        self.inner.stop().await
    }

    /// Destroy the sandbox: tear down project mount, stop gateway, then destroy the VM.
    pub async fn destroy(mut self) -> Result<()> {
        // Stop the gateway gracefully.
        if let Some(ref gw) = self.gateway {
            if let Err(e) = gw.stop().await {
                warn!("Failed to gracefully stop gateway: {}", e);
            }
        }

        // Teardown project mount (auto-commit and remove clone) before VM destroy.
        if let Some(mut pm) = self.project_mount.take() {
            if let Err(e) = pm.teardown() {
                tracing::warn!("Failed to teardown project mount: {}", e);
            }
        }

        // Clean up SSH keys.
        if let Some(ref key_path) = self.ssh_key_path {
            gateway::cleanup_ssh_keys(key_path);
        }

        self.inner.destroy().await
    }
}

impl Deref for Sandbox {
    type Target = runtime::Sandbox;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Sandbox {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Allocate an ephemeral port by binding to `127.0.0.1:0` and returning the assigned port.
fn allocate_ephemeral_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
}

/// Detect and set up the project mount declared by `config.project`.
///
/// Mutates `config.mounts` to include the virtiofs mount. Returns the
/// `ProjectMount` so the caller can manage its lifecycle (teardown on destroy).
pub fn setup_project_mount(
    config: &mut SandboxConfig,
    on_progress: Option<&ProgressFn>,
) -> Result<Option<ProjectMount>> {
    let Some(proj_config) = config.project.clone() else {
        return Ok(None);
    };

    if let Some(cb) = on_progress {
        cb("Setting up project...");
    }

    // Generate a per-mount id independent of the VM's uuid — ProjectMount uses
    // this purely for branch/clone naming and doesn't need to match sandbox.id.
    let id = uuid::Uuid::new_v4().to_string();

    let mut pm = ProjectMount::detect(&proj_config.path).map_err(|e| {
        error!(mount_id = %id, "Project detection failed: {}", e);
        runtime::Error::SandboxCreationFailed(format!("Project detection failed: {}", e))
    })?;

    let strategy = match &proj_config.branch {
        Some(name) => BranchStrategy::Named(name.clone()),
        None => BranchStrategy::Auto,
    };

    let wt_path = if proj_config.auto_sync {
        pm.setup(&id, &strategy).map_err(|e| {
            error!(mount_id = %id, "Project clone setup failed: {}", e);
            runtime::Error::SandboxCreationFailed(format!("Project clone setup failed: {}", e))
        })?
    } else {
        pm.setup_deferred(&id, &strategy).map_err(|e| {
            error!(mount_id = %id, "Project clone setup (deferred) failed: {}", e);
            runtime::Error::SandboxCreationFailed(format!(
                "Project clone setup (deferred) failed: {}",
                e
            ))
        })?
    };

    info!(
        "Project mounted: {} -> {}",
        wt_path.display(),
        proj_config.mount_point
    );

    if let Some(mount) = pm.mount_config(&proj_config.mount_point) {
        config.mounts.push(mount);
    }

    Ok(Some(pm))
}
