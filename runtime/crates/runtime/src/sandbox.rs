//! Sandbox management
//!
//! High-level API for creating and managing sandboxed execution environments.

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
use tracing::{debug, error, info, warn};

/// Bind to port 0 to let the OS assign an ephemeral port, then return it.
/// The listener is dropped immediately so the port is free for gvproxy/libkrun to bind.
fn allocate_ephemeral_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
}

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
    /// Whether gateway mode is active (set by sandbox layer via config.gateway)
    has_gateway: bool,
    /// Gateway address for HTTP communication (host:port on Unix, guest_ip:port on Windows)
    gateway_addr: Option<String>,
    /// Host port for SSH access
    ssh_port: Option<u16>,
    /// Path to SSH private key
    ssh_key_path: Option<std::path::PathBuf>,
    /// HCS VM identity for HvSocket connections (Windows only).
    #[cfg(target_os = "windows")]
    hcs_vm_id: Option<String>,
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
        // APFS clonefile (macOS) or recursive copy (Linux/Windows).
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
            has_gateway: false,
            gateway_addr: None,
            ssh_port: None,
            ssh_key_path: None,
            #[cfg(target_os = "windows")]
            hcs_vm_id: None,
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
                has_gateway: false,
                gateway_addr: None,
                ssh_port: None,
                ssh_key_path: None,
                #[cfg(target_os = "windows")]
                hcs_vm_id: None,
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

    /// Get the creation timestamp
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// Start the sandbox
    ///
    /// Creates the VM via libkrun, detects gateway/SSH mode, injects SSH keys,
    /// boots the VM, and runs health checks.
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

        // --- Pre-boot: detect gateway mode and set up SSH ---

        let rootfs_path = &bundle.rootfs_path;
        // Gateway mode is set by the sandbox layer via config.gateway.
        // The runtime does not inspect rootfs for agent-gateway or init scripts.
        let has_gateway = self.config.gateway;

        if has_gateway {
            info!(
                "Gateway mode enabled for sandbox '{}' — persistent VM mode",
                self.id
            );

            // Ensure port mappings for gateway (8080) and SSH (22) so HCN networking
            // gets set up. Without port mappings, the HCS builder skips HCN entirely
            // and the VM has no network interface.
            //
            // Host ports are allocated dynamically (bind to :0) to avoid:
            //   - Permission errors (port 22 is privileged on macOS/Linux)
            //   - Conflicts between multiple concurrent sandboxes
            use crate::config::PortMapping;
            let needs_8080 = !self.config.network.port_mappings.iter().any(|p| p.container_port == 8080);
            let needs_22 = !self.config.network.port_mappings.iter().any(|p| p.container_port == 22);
            if needs_8080 {
                let host_port = allocate_ephemeral_port().unwrap_or(8080);
                info!("Gateway port mapping: host:{} -> guest:8080", host_port);
                self.config.network.port_mappings.push(PortMapping {
                    host_port,
                    container_port: 8080,
                    protocol: "tcp".to_string(),
                });
            }
            if needs_22 {
                let host_port = allocate_ephemeral_port().unwrap_or(2222);
                info!("SSH port mapping: host:{} -> guest:22", host_port);
                self.config.network.port_mappings.push(PortMapping {
                    host_port,
                    container_port: 22,
                    protocol: "tcp".to_string(),
                });
            }

            // Generate SSH keys and inject pubkey into rootfs
            match crate::ssh::generate_ssh_keys(&self.id) {
                Ok((key_path, pubkey)) => {
                    info!("SSH keys generated for sandbox '{}': {}", self.id, key_path.display());
                    // Inject into rootfs (works on macOS/Linux with real permissions)
                    if let Err(e) = crate::ssh::inject_pubkey_into_rootfs(rootfs_path, &pubkey) {
                        warn!("Failed to inject SSH pubkey into rootfs: {} (SSH may not work)", e);
                    }
                    // Pass pubkey to the builder via config so it gets appended to
                    // the kernel cmdline as nanosb.ssh_key=... In 9P mode (Windows HCS)
                    // rootfs permissions are 0777; the init script reads the key from
                    // /proc/cmdline after creating tmpfs overlays with proper perms.
                    self.config.ssh_pubkey = Some(pubkey.trim().to_string());
                    self.ssh_key_path = Some(key_path);
                }
                Err(e) => {
                    warn!("Failed to generate SSH keys for sandbox '{}': {} (SSH access unavailable)", self.id, e);
                }
            }
        }

        self.has_gateway = has_gateway;

        // --- Create and start the VM ---

        // Timeout must accommodate first-run rootfs creation (~2 min on Windows)
        // plus VM boot + health checks.
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

        // --- Post-boot: set up gateway address and health checks ---

        if has_gateway {
            let guest_ip = runtime.guest_ip(&self.id);

            // On Windows, use HvSocket (AF_HYPERV) for ALL host↔guest communication
            // (both gateway HTTP and SSH), bypassing HCN NAT TCP convergence delay (~60s).
            #[cfg(target_os = "windows")]
            {
                self.hcs_vm_id = runtime.hcs_vm_id(&self.id);
                if let Some(ref vm_id) = self.hcs_vm_id {
                    // Gateway HTTP goes over HvSocket directly (no TCP proxy needed).
                    self.gateway_addr = Some("127.0.0.1:8080".to_string());

                    // SSH goes over HvSocket via a local TCP→HvSocket proxy.
                    // The proxy binds a random local port and forwards through HvSocket
                    // to the guest's SSH daemon (vsock port 50022 → guest TCP 22).
                    let port = crate::hvsocket::start_ssh_proxy(vm_id)
                        .map_err(|e| Error::SandboxCreationFailed(
                            format!("Failed to start SSH HvSocket proxy: {}", e)
                        ))?;
                    self.ssh_port = Some(port);
                    info!("Gateway: HvSocket, SSH: HvSocket via 127.0.0.1:{}", port);

                    // Start host-side DNS relay (guest vsock 50053 → 8.8.8.8:53)
                    crate::hvsocket::start_dns_proxy(vm_id)
                        .map_err(|e| Error::SandboxCreationFailed(
                            format!("Failed to start DNS HvSocket proxy: {}", e)
                        ))?;

                    // Start host-side TCP connect relay (guest vsock 50080 → internet)
                    crate::hvsocket::start_tcp_proxy(vm_id)
                        .map_err(|e| Error::SandboxCreationFailed(
                            format!("Failed to start TCP HvSocket proxy: {}", e)
                        ))?;

                    // Start dynamic inbound port forwarders for each user-requested
                    // port mapping (excluding 8080/22 which have dedicated paths).
                    // host:host_port → guest:container_port via vsock 50090.
                    for mapping in &self.config.network.port_mappings {
                        if mapping.container_port == 8080 || mapping.container_port == 22 {
                            continue;
                        }
                        match crate::hvsocket::start_inbound_port_forwarder(
                            vm_id, mapping.host_port, mapping.container_port,
                        ) {
                            Ok(p) => info!(
                                "Port forward: 127.0.0.1:{} -> guest:{}",
                                p, mapping.container_port
                            ),
                            Err(e) => warn!(
                                "Port forward host:{} -> guest:{} failed: {}",
                                mapping.host_port, mapping.container_port, e
                            ),
                        }
                    }
                } else {
                    return Err(Error::SandboxCreationFailed(
                        "HCS VM ID not available — cannot establish HvSocket communication".to_string()
                    ));
                }
            }

            #[cfg(not(target_os = "windows"))]
            {
                // With gvproxy, the host reaches guest services via 127.0.0.1:<host_port>
                // (the ephemeral host port allocated above for each container_port).
                // guest_ip routing is unavailable when gvproxy handles networking, so we
                // derive endpoints from the port_mappings table instead.
                let find_host_port = |container: u16| {
                    self.config
                        .network
                        .port_mappings
                        .iter()
                        .find(|p| p.container_port == container)
                        .map(|p| p.host_port)
                };

                if let Some(host_port) = find_host_port(8080) {
                    self.gateway_addr = Some(format!("127.0.0.1:{}", host_port));
                }
                if let Some(host_port) = find_host_port(22) {
                    self.ssh_port = Some(host_port);
                }

                match (&self.gateway_addr, self.ssh_port) {
                    (Some(addr), Some(ssh)) => {
                        info!("Gateway address: {}, SSH: 127.0.0.1:{}", addr, ssh);
                    }
                    _ => {
                        // Fallback to guest_ip path for non-gvproxy backends (e.g. TSI).
                        if let Some(ref ip) = guest_ip {
                            if self.gateway_addr.is_none() {
                                self.gateway_addr = Some(format!("{}:8080", ip));
                            }
                            if self.ssh_port.is_none() {
                                self.ssh_port = Some(22);
                            }
                            info!("Gateway address: {}:8080, SSH: {}:22 (guest_ip path)", ip, ip);
                        }
                    }
                }
            }

            // Health check: wait for gateway HTTP /health
            if let Some(ref addr) = self.gateway_addr {
                self.wait_for_gateway_health(addr, runtime).await?;
            }
        }

        self.status = SandboxStatus::Running;
        info!("Sandbox {} is now running", self.id);
        Ok(())
    }

    /// Wait for the gateway HTTP /health endpoint to become available.
    ///
    /// On Windows with HvSocket: uses console detection for readiness awareness,
    /// then immediately tries HvSocket health check (bypasses HCN NAT ~60s delay).
    /// Falls back to TCP if HvSocket is not available.
    /// On other platforms: uses TCP health check polling.
    async fn wait_for_gateway_health(&self, addr: &str, runtime: &Runtime) -> Result<()> {
        let deadline = std::time::Instant::now() + Duration::from_secs(240);
        let start = std::time::Instant::now();
        let mut console_detected = false;

        // On Windows with HvSocket, skip TCP health checks entirely — there is
        // no HCN NAT, so TCP to 127.0.0.1:8080 will never route to the guest.
        #[cfg(target_os = "windows")]
        let use_hvsocket = self.hcs_vm_id.is_some();
        #[cfg(not(target_os = "windows"))]
        let use_hvsocket = false;

        info!("Waiting for gateway health at {}...", addr);

        loop {
            if std::time::Instant::now() > deadline {
                error!(sandbox_id = %self.id, "Gateway health check timed out after 240s at {}", addr);
                return Err(Error::SandboxCreationFailed(format!(
                    "Gateway health check timed out after 240s at {}",
                    addr
                )));
            }

            // Fail fast if the VM process has died (e.g., rootfs setup failed).
            if !runtime.is_vm_running(&self.id) {
                let elapsed = start.elapsed().as_secs_f32();
                error!(sandbox_id = %self.id, "VM process died after {:.1}s — aborting health check", elapsed);
                return Err(Error::SandboxCreationFailed(
                    "VM process exited before gateway became healthy".to_string()
                ));
            }

            // Check console-based detection (Windows only — instant).
            if !console_detected && runtime.is_gateway_ready(&self.id) {
                let elapsed = start.elapsed().as_secs_f32();
                info!("Gateway started (detected from console at {:.1}s), trying HvSocket...", elapsed);
                console_detected = true;
            }

            // On Windows, try HvSocket (the only path — no HCN NAT).
            #[cfg(target_os = "windows")]
            if console_detected && use_hvsocket {
                if let Some(ref vm_id) = self.hcs_vm_id {
                    match crate::http::http_get_hvsocket(vm_id, "/health") {
                        Ok((status, _body)) if status == 200 => {
                            let elapsed = start.elapsed().as_secs_f32();
                            info!("Gateway health check passed via HvSocket for sandbox '{}' in {:.1}s", self.id, elapsed);
                            return Ok(());
                        }
                        Ok((status, _)) => {
                            debug!("HvSocket health check returned status {}, retrying...", status);
                        }
                        Err(e) => {
                            debug!("HvSocket not ready yet: {}", e);
                        }
                    }
                }
            }

            // TCP health check — only used on non-Windows or when HvSocket is unavailable.
            if !use_hvsocket {
                match crate::http::http_get(addr, "/health") {
                    Ok((status, _body)) if status == 200 => {
                        let elapsed = start.elapsed().as_secs_f32();
                        info!("Gateway health check passed via TCP for sandbox '{}' in {:.1}s", self.id, elapsed);
                        return Ok(());
                    }
                    _ => {
                        if console_detected {
                            debug!("Gateway running but TCP not yet routable, retrying...");
                        } else {
                            debug!("Gateway not reachable yet, retrying...");
                        }
                    }
                }
            }

            // Poll faster once we know the gateway is running
            let sleep_ms = if console_detected { 200 } else { 500 };
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
        }
    }

    /// Build the JSON body for a gateway exec request.
    fn gateway_exec_body(
        command: &str,
        args: &[&str],
        env: &std::collections::HashMap<String, String>,
        timeout_secs: u32,
    ) -> String {
        serde_json::json!({
            "command": command,
            "args": args,
            "env": env,
            "timeout": timeout_secs,
        })
        .to_string()
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
            error!(sandbox_id = %self.id, "Cannot exec in sandbox with {:?} status", self.status);
            return Err(Error::InvalidState(format!(
                "Cannot exec in sandbox with {:?} status",
                self.status
            )));
        }

        let timeout_secs = options.timeout_secs.unwrap_or(self.config.timeout_secs);

        // Merge sandbox config env with exec-specific env (exec overrides config)
        let mut merged_env = self.config.env.clone();
        merged_env.extend(options.env);

        let start = std::time::Instant::now();

        // Route through gateway if available (persistent VM mode)
        if let Some(ref addr) = self.gateway_addr {
            debug!("Executing via gateway at {}: {} {:?}", addr, command, args);
            let json_body = Self::gateway_exec_body(command, args, &merged_env, timeout_secs);
            let addr = addr.clone();
            #[cfg(target_os = "windows")]
            let hcs_vm_id = self.hcs_vm_id.clone();

            let result = tokio::task::spawn_blocking(move || {
                let stdout = std::sync::Mutex::new(String::new());
                let stderr = std::sync::Mutex::new(String::new());

                let on_output = |text: &str, is_stderr: bool| {
                    if is_stderr {
                        stderr.lock().unwrap().push_str(text);
                    } else {
                        stdout.lock().unwrap().push_str(text);
                    }
                };

                // On Windows, use HvSocket exclusively (no HCN NAT).
                #[cfg(target_os = "windows")]
                {
                    let vm_id = hcs_vm_id.as_ref().ok_or_else(|| {
                        Error::ExecFailed("HCS VM ID not available for HvSocket exec".to_string())
                    })?;
                    let code = crate::http::http_post_sse_hvsocket(
                        vm_id, "/api/v1/exec", &json_body, &on_output,
                    )
                    .map_err(|e| Error::ExecFailed(format!("HvSocket exec failed: {}", e)))?;
                    return Ok::<_, Error>(ExecResult {
                        exit_code: code,
                        stdout: stdout.into_inner().unwrap(),
                        stderr: stderr.into_inner().unwrap(),
                        duration_ms: 0,
                    });
                }

                #[cfg(not(target_os = "windows"))]
                {
                    let exit_code = crate::http::http_post_sse(
                        &addr,
                        "/api/v1/exec",
                        &json_body,
                        on_output,
                    )
                    .map_err(|e| Error::ExecFailed(format!("Gateway exec failed: {}", e)))?;

                    Ok::<_, Error>(ExecResult {
                        exit_code,
                        stdout: stdout.into_inner().unwrap(),
                        stderr: stderr.into_inner().unwrap(),
                        duration_ms: 0,
                    })
                }
            })
            .await
            .map_err(|e| Error::ExecFailed(format!("Gateway exec task failed: {}", e)))??;

            return Ok(ExecResult {
                duration_ms: start.elapsed().as_millis() as u64,
                ..result
            });
        }

        // Single-path: exec is only available via the in-guest agent-gateway.
        // The sandbox layer is responsible for ensuring the VM runs agent-gateway
        // as its main process; if it didn't, gateway_addr is None and we error.
        error!(sandbox_id = %self.id, "exec requires agent-gateway; gateway_addr is None");
        Err(Error::ExecFailed(
            "gateway not available — sandbox must be configured to run agent-gateway"
                .to_string(),
        ))
    }

    /// Execute a command with streaming output
    pub async fn exec_stream<F>(&self, command: &str, args: &[&str], on_output: F) -> Result<i32>
    where
        F: Fn(OutputChunk) + Send + Sync + 'static,
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
        F: Fn(OutputChunk) + Send + Sync + 'static,
    {
        if self.status != SandboxStatus::Running {
            error!(sandbox_id = %self.id, "Cannot exec (streaming) in sandbox with {:?} status", self.status);
            return Err(Error::InvalidState(format!(
                "Cannot exec in sandbox with {:?} status",
                self.status
            )));
        }

        let timeout_secs = options.timeout_secs.unwrap_or(self.config.timeout_secs);

        // Merge sandbox config env with exec-specific env (exec overrides config)
        let mut merged_env = self.config.env.clone();
        merged_env.extend(options.env);

        // Route through gateway if available (persistent VM mode)
        if let Some(ref addr) = self.gateway_addr {
            debug!("Executing (streaming) via gateway at {}: {} {:?}", addr, command, args);
            // Diagnostic: log command + env keys so we can verify NANOSB_PROMPT
            // and other expected vars are actually being sent over the wire.
            let env_keys: Vec<&str> = merged_env.keys().map(|s| s.as_str()).collect();
            let prompt_present = merged_env.get("NANOSB_PROMPT")
                .map(|v| format!("len={}", v.len()))
                .unwrap_or_else(|| "MISSING".to_string());
            info!(
                "[gateway_exec] command={} args={:?} env_keys={:?} NANOSB_PROMPT={}",
                command, args, env_keys, prompt_present,
            );
            let json_body = Self::gateway_exec_body(command, args, &merged_env, timeout_secs);
            let addr = addr.clone();
            #[cfg(target_os = "windows")]
            let hcs_vm_id_stream = self.hcs_vm_id.clone();

            // Wrap the synchronous blocking SSE read in spawn_blocking so that
            // (a) we don't block a tokio worker thread and (b) the runtime drop
            // on shutdown doesn't wait for a socket read that might be parked
            // in the kernel. The blocking-pool thread is detached and killed
            // when the process exits. `F: 'static` lets us move `on_output`
            // into the blocking task.
            let on_output = std::sync::Arc::new(on_output);

            // On Windows, use HvSocket exclusively (no HCN NAT).
            #[cfg(target_os = "windows")]
            {
                let vm_id = hcs_vm_id_stream.as_ref().ok_or_else(|| {
                    Error::ExecFailed("HCS VM ID not available for HvSocket exec_stream".to_string())
                })?.clone();
                let on_output_blocking = on_output.clone();
                let code = tokio::task::spawn_blocking(move || {
                    let on_sse = |text: &str, is_stderr: bool| {
                        on_output_blocking(OutputChunk {
                            stream: if is_stderr { Stream::Stderr } else { Stream::Stdout },
                            data: text.to_string(),
                            timestamp: Utc::now(),
                        });
                    };
                    crate::http::http_post_sse_hvsocket(
                        &vm_id, "/api/v1/exec", &json_body, &on_sse,
                    )
                })
                .await
                .map_err(|e| Error::ExecFailed(format!("HvSocket exec_stream task join failed: {}", e)))?
                .map_err(|e| Error::ExecFailed(format!("HvSocket exec_stream failed: {}", e)))?;
                return Ok(code);
            }

            #[cfg(not(target_os = "windows"))]
            {
                let on_output_blocking = on_output.clone();
                let exit_code = tokio::task::spawn_blocking(move || {
                    let on_sse = |text: &str, is_stderr: bool| {
                        on_output_blocking(OutputChunk {
                            stream: if is_stderr { Stream::Stderr } else { Stream::Stdout },
                            data: text.to_string(),
                            timestamp: Utc::now(),
                        });
                    };
                    crate::http::http_post_sse(
                        &addr,
                        "/api/v1/exec",
                        &json_body,
                        on_sse,
                    )
                })
                .await
                .map_err(|e| Error::ExecFailed(format!("Gateway exec task join failed: {}", e)))?
                .map_err(|e| Error::ExecFailed(format!("Gateway exec failed: {}", e)))?;
                return Ok(exit_code);
            }
        }

        // Single-path: streaming exec is only available via the in-guest
        // agent-gateway. The sandbox layer is responsible for ensuring the VM
        // runs agent-gateway as its main process; if it didn't, gateway_addr
        // is None and we error here rather than spawning an ephemeral VM.
        error!(sandbox_id = %self.id, "exec_stream requires agent-gateway; gateway_addr is None");
        Err(Error::ExecFailed(
            "gateway not available — sandbox must be configured to run agent-gateway"
                .to_string(),
        ))
    }

    /// Check if this sandbox is in persistent (gateway) mode.
    pub fn is_persistent(&self) -> bool {
        self.has_gateway && self.gateway_addr.is_some()
    }

    /// Dynamically forward a guest port to the same host port via gvproxy.
    pub fn expose_port(&self, port: u16) -> std::result::Result<(), String> {
        self.runtime
            .as_ref()
            .ok_or_else(|| "no runtime".to_string())
            .and_then(|r| r.expose_port(&self.id, port))
    }

    /// Get the SSH host port for this sandbox (if available).
    pub fn ssh_port(&self) -> Option<u16> {
        self.ssh_port
    }

    /// Get the SSH private key path for this sandbox (if available).
    pub fn ssh_key_path(&self) -> Option<std::path::PathBuf> {
        self.ssh_key_path.clone()
    }

    /// Get a ready-to-use SSH command string for connecting to this sandbox.
    pub fn ssh_command(&self) -> Option<String> {
        let port = self.ssh_port?;
        let key = self.ssh_key_path.as_ref()?;
        Some(format!(
            "ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -i {} -p {} root@127.0.0.1",
            key.display(),
            port
        ))
    }

    /// Get the guest IP address for this sandbox.
    pub fn guest_ip(&self) -> Option<String> {
        self.runtime
            .as_ref()
            .and_then(|r| r.guest_ip(&self.id))
    }

    /// Get the gateway address for HTTP communication (if in persistent mode).
    /// Linux/macOS only — on Windows we talk to the gateway over HvSocket.
    #[cfg(not(target_os = "windows"))]
    fn require_gateway_addr(&self) -> Result<&str> {
        if self.status != SandboxStatus::Running {
            return Err(Error::InvalidState(
                "Sandbox must be running for HTTP operations".to_string(),
            ));
        }
        self.gateway_addr.as_deref().ok_or_else(|| {
            Error::ExecFailed(
                "HTTP operations require a persistent VM with agent-gateway".to_string(),
            )
        })
    }

    /// Send a generic HTTP GET request to the gateway process inside the sandbox.
    /// On Windows, uses HvSocket exclusively (no HCN NAT).
    pub fn gateway_http_get(&self, path: &str) -> Result<(u16, String)> {
        #[cfg(target_os = "windows")]
        {
            let vm_id = self.hcs_vm_id.as_ref().ok_or_else(|| {
                Error::ExecFailed("HCS VM ID not available for HvSocket".to_string())
            })?;
            return crate::http::http_get_hvsocket(vm_id, path).map_err(Error::ExecFailed);
        }
        #[cfg(not(target_os = "windows"))]
        {
            let addr = self.require_gateway_addr()?;
            crate::http::http_get(addr, path).map_err(Error::ExecFailed)
        }
    }

    /// Send a generic HTTP POST request to the gateway process inside the sandbox.
    pub fn gateway_http_post(&self, path: &str, json_body: &str) -> Result<(u16, String)> {
        #[cfg(target_os = "windows")]
        {
            let vm_id = self.hcs_vm_id.as_ref().ok_or_else(|| {
                Error::ExecFailed("HCS VM ID not available for HvSocket".to_string())
            })?;
            return crate::http::http_post_hvsocket(vm_id, path, json_body).map_err(Error::ExecFailed);
        }
        #[cfg(not(target_os = "windows"))]
        {
            let addr = self.require_gateway_addr()?;
            crate::http::http_post(addr, path, json_body).map_err(Error::ExecFailed)
        }
    }

    /// Send a generic HTTP DELETE request to the gateway process inside the sandbox.
    pub fn gateway_http_delete(&self, path: &str) -> Result<(u16, String)> {
        #[cfg(target_os = "windows")]
        {
            let vm_id = self.hcs_vm_id.as_ref().ok_or_else(|| {
                Error::ExecFailed("HCS VM ID not available for HvSocket".to_string())
            })?;
            return crate::http::http_delete_hvsocket(vm_id, path).map_err(Error::ExecFailed);
        }
        #[cfg(not(target_os = "windows"))]
        {
            let addr = self.require_gateway_addr()?;
            crate::http::http_delete(addr, path).map_err(Error::ExecFailed)
        }
    }

    /// Send a generic HTTP POST with SSE streaming to the gateway process inside the sandbox.
    pub fn gateway_http_post_sse<F>(&self, path: &str, json_body: &str, on_output: F) -> Result<i32>
    where
        F: Fn(&str, bool) + Send + Sync,
    {
        #[cfg(target_os = "windows")]
        {
            let vm_id = self.hcs_vm_id.as_ref().ok_or_else(|| {
                Error::ExecFailed("HCS VM ID not available for HvSocket".to_string())
            })?;
            return crate::http::http_post_sse_hvsocket(vm_id, path, json_body, &on_output).map_err(Error::ExecFailed);
        }
        #[cfg(not(target_os = "windows"))]
        {
            let addr = self.require_gateway_addr()?;
            crate::http::http_post_sse(addr, path, json_body, on_output).map_err(Error::ExecFailed)
        }
    }

    /// Stop the sandbox
    pub async fn stop(&mut self) -> Result<()> {
        if self.status != SandboxStatus::Running {
            return Ok(()); // Already stopped
        }

        info!("Stopping sandbox {}", self.id);

        // If in persistent mode, send graceful stop request to gateway
        if self.gateway_addr.is_some() {
            // Try HvSocket first on Windows (no HCN NAT available).
            #[cfg(target_os = "windows")]
            if let Some(ref vm_id) = self.hcs_vm_id {
                let vm_id = vm_id.clone();
                info!("Sending stop request via HvSocket for sandbox '{}'", self.id);
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = crate::http::http_post_hvsocket(&vm_id, "/api/v1/stop", "{}");
                })
                .await;
            }
            // TCP fallback for non-Windows platforms.
            #[cfg(not(target_os = "windows"))]
            {
                let addr = self.gateway_addr.as_ref().unwrap().clone();
                info!("Sending stop request to gateway at {} for sandbox '{}'", addr, self.id);
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = crate::http::http_post(&addr, "/api/v1/stop", "{}");
                })
                .await;
            }
            // Brief wait for graceful shutdown
            tokio::time::sleep(Duration::from_secs(2)).await;
        }

        if let Some(ref runtime) = self.runtime {
            runtime.kill(&self.id).await?;
        }

        self.gateway_addr = None;
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

        // Stop if running — must not short-circuit on error, otherwise
        // subsequent cleanup (SSH keys, project mount, bundle) is skipped.
        if self.status == SandboxStatus::Running {
            if let Err(e) = self.stop().await {
                warn!("Failed to stop sandbox {} during destroy: {}", self.id, e);
            }
        }

        // Clean up SSH keys
        if let Some(ref key_path) = self.ssh_key_path {
            crate::ssh::cleanup_ssh_keys(key_path);
        }

        // Delete from runtime — don't short-circuit, bundle cleanup must still run.
        if let Some(ref runtime) = self.runtime {
            if let Err(e) = runtime.delete(&self.id).await {
                warn!("Failed to delete sandbox {} from runtime: {}", self.id, e);
            }
        }

        // Remove the bundle directory.
        // On Windows, rootfs may be an NTFS junction to the golden rootfs.
        // We must remove the junction first (without following it) to avoid
        // deleting the shared golden rootfs.
        if let Some(bundle) = &self.bundle {
            if bundle.path.exists() {
                #[cfg(target_os = "windows")]
                {
                    let rootfs = bundle.path.join("rootfs");
                    if rootfs.exists() {
                        // remove_dir removes a junction point itself (not its target)
                        // unlike remove_dir_all which follows into the target.
                        let _ = std::fs::remove_dir(&rootfs);
                    }
                }
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
            has_gateway: false,
            gateway_addr: None,
            ssh_port: None,
            ssh_key_path: None,
            #[cfg(target_os = "windows")]
            hcs_vm_id: None,
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

    #[test]
    fn test_gateway_http_get_requires_running_state() {
        let sandbox = Sandbox::new_test("test-get", SandboxStatus::Ready);
        let result = sandbox.gateway_http_get("/health");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidState(_)));
    }

    #[test]
    fn test_gateway_http_post_requires_running_state() {
        let sandbox = Sandbox::new_test("test-post", SandboxStatus::Stopped);
        let result = sandbox.gateway_http_post("/api/test", "{}");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidState(_)));
    }

    #[test]
    fn test_gateway_http_delete_requires_running_state() {
        let sandbox = Sandbox::new_test("test-delete", SandboxStatus::Creating);
        let result = sandbox.gateway_http_delete("/api/test");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidState(_)));
    }

    #[test]
    fn test_gateway_http_post_sse_requires_running_state() {
        let sandbox = Sandbox::new_test("test-sse", SandboxStatus::Ready);
        let result = sandbox.gateway_http_post_sse("/api/stream", "{}", |_data, _is_stderr| {});
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidState(_)));
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
