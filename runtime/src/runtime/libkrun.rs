//! Direct libkrun FFI Runtime Backend for macOS and Linux
//!
//! This backend calls libkrun's C API directly via FFI instead of going through
//! CLI runtimes. Key advantages:
//!
//! - **Networking**: Uses gvproxy for full outbound connectivity via virtio-net,
//!   with automatic fallback to TSI when gvproxy is not available
//! - **Linux**: Direct VM management via KVM without needing CLI binaries
//! - **Both**: No buildah dependency -- rootfs is prepared by the Sandbox orchestrator
//!   using the pure-Rust `ImageManager` (`pull()` + `create_rootfs()`)
//!
//! Architecture:
//! - `handles_image_pull()` returns `false` -- the Sandbox orchestrator pulls the OCI
//!   image and creates a rootfs via `ImageManager`, then passes the bundle_path here
//! - Forks a child process for each command execution (`krun_start_enter` never returns)
//! - Parent process captures stdout/stderr via pipes
//! - gvproxy sidecar process provides user-mode networking when available

use super::ffi;
use super::gvproxy::{GvproxyInstance, GvproxyManager};
use super::ExecOutput;
use crate::config::{MountType, NetworkScope, SandboxConfig};
use crate::error::{Error, Result};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read as IoRead, Write as IoWrite};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// State tracked per sandbox for the libkrun backend
struct SandboxState {
    /// Path to the rootfs directory (derived from bundle_path/rootfs)
    rootfs_path: PathBuf,
    /// Number of vCPUs
    cpus: u32,
    /// Memory in MiB
    memory_mb: u32,
    /// DNS servers
    dns: Vec<String>,
    /// Network scope for TSI
    network_scope: NetworkScope,
    /// Port mappings (host:guest)
    port_mappings: Vec<(u16, u16)>,
    /// Mounts to add as virtiofs
    mounts: Vec<(String, String)>,
    /// gvproxy instance for virtio-net networking (None = TSI fallback)
    gvproxy: Option<GvproxyInstance>,
    /// Host-side port mapped to guest's agent-gateway (port 8080)
    gateway_port: Option<u16>,
    /// PID of the forked child running the persistent VM
    vm_pid: Option<i32>,
    /// Whether the rootfs contains /usr/local/bin/agent-gateway (persistent mode)
    has_gateway: bool,
}

/// Direct libkrun FFI runtime for macOS and Linux
///
/// This backend calls libkrun's C API directly. Networking uses gvproxy
/// (virtio-net) when available for full outbound connectivity, with automatic
/// fallback to TSI. On Linux it uses KVM directly.
///
/// Rootfs preparation is handled externally by the Sandbox orchestrator using
/// the pure-Rust `ImageManager`, eliminating the buildah dependency entirely.
pub struct LibkrunRuntime {
    /// Tracked sandboxes by ID (behind Mutex for interior mutability,
    /// so RuntimeBackend can call create/destroy with &self)
    sandboxes: std::sync::Mutex<HashMap<String, SandboxState>>,
}

impl LibkrunRuntime {
    /// Create a new libkrun runtime instance
    pub async fn new() -> Result<Self> {
        // Verify libkrun is available
        if !Self::is_available() {
            return Err(Error::RuntimeNotAvailable(
                Self::not_available_message().to_string(),
            ));
        }

        // Set libkrun log level to Info
        if let Err(e) = ffi::set_log_level(3) {
            warn!("Failed to set libkrun log level: {}", e);
        }

        Ok(Self {
            sandboxes: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Check if libkrun is available on this system
    pub fn is_available() -> bool {
        #[cfg(target_os = "macos")]
        {
            Path::new("/opt/homebrew/lib/libkrun.dylib").exists()
        }

        #[cfg(target_os = "linux")]
        {
            Path::new("/usr/lib/libkrun.so").exists()
                || Path::new("/usr/lib64/libkrun.so").exists()
                || Path::new("/usr/local/lib/libkrun.so").exists()
                || Path::new("/usr/lib/x86_64-linux-gnu/libkrun.so").exists()
                || Path::new("/usr/lib/aarch64-linux-gnu/libkrun.so").exists()
        }
    }

    /// Get the error message for when libkrun is not available
    fn not_available_message() -> &'static str {
        #[cfg(target_os = "macos")]
        {
            "libkrun not found. Expected libkrun.dylib at /opt/homebrew/lib/. \
             Install with: brew tap slp/krun && brew install libkrun"
        }

        #[cfg(target_os = "linux")]
        {
            "libkrun not found. Expected libkrun.so in /usr/lib/, /usr/lib64/, or /usr/local/lib/. \
             Install from: https://github.com/containers/libkrun"
        }
    }

    /// Fork a child process, configure libkrun, and run a command in the VM.
    /// Returns (exit_code, stdout, stderr).
    fn run_in_vm(
        rootfs_path: &str,
        cpus: u32,
        memory_mb: u32,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
        network_scope: &NetworkScope,
        port_mappings: &[(u16, u16)],
        mounts: &[(String, String)],
        dns: &[String],
        gvproxy_socket: Option<&str>,
    ) -> std::result::Result<(i32, String, String), String> {
        // Create pipes for stdout and stderr capture
        let (stdout_read_fd, stdout_write_fd) = Self::create_pipe()?;
        let (stderr_read_fd, stderr_write_fd) = Self::create_pipe()?;

        // Fork
        let pid = unsafe { libc::fork() };

        if pid < 0 {
            // Fork failed -- close all fds
            unsafe {
                libc::close(stdout_read_fd);
                libc::close(stdout_write_fd);
                libc::close(stderr_read_fd);
                libc::close(stderr_write_fd);
            }
            return Err("fork() failed".to_string());
        }

        if pid == 0 {
            // ============ CHILD PROCESS ============
            // Close read ends
            unsafe {
                libc::close(stdout_read_fd);
                libc::close(stderr_read_fd);
            }

            // Redirect stdout/stderr to pipes
            unsafe {
                libc::dup2(stdout_write_fd, libc::STDOUT_FILENO);
                libc::dup2(stderr_write_fd, libc::STDERR_FILENO);
                libc::close(stdout_write_fd);
                libc::close(stderr_write_fd);
            }

            // Configure and start the VM
            let result = Self::configure_and_start_vm(
                rootfs_path,
                cpus,
                memory_mb,
                command,
                args,
                workdir,
                env,
                network_scope,
                port_mappings,
                mounts,
                dns,
                gvproxy_socket,
            );

            // If we get here, krun_start_enter failed
            if let Err(e) = result {
                eprintln!("libkrun VM start failed: {}", e);
            }
            unsafe { libc::_exit(1) };
        }

        // ============ PARENT PROCESS ============
        // Close write ends
        unsafe {
            libc::close(stdout_write_fd);
            libc::close(stderr_write_fd);
        }

        // Read stdout and stderr from pipes
        let stdout = Self::read_fd(stdout_read_fd);
        let stderr = Self::read_fd(stderr_read_fd);

        // Close read ends
        unsafe {
            libc::close(stdout_read_fd);
            libc::close(stderr_read_fd);
        }

        // Wait for child
        let mut status: libc::c_int = 0;
        unsafe {
            libc::waitpid(pid, &mut status, 0);
        }

        let exit_code = if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else if libc::WIFSIGNALED(status) {
            -(libc::WTERMSIG(status) as i32)
        } else {
            -1
        };

        Ok((exit_code, stdout, stderr))
    }

    /// Fork a child process and stream output via an mpsc channel.
    /// Returns the exit code. Output lines are sent as `(text, is_stderr)` tuples.
    fn run_in_vm_with_channel(
        rootfs_path: &str,
        cpus: u32,
        memory_mb: u32,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
        network_scope: &NetworkScope,
        port_mappings: &[(u16, u16)],
        mounts: &[(String, String)],
        dns: &[String],
        gvproxy_socket: Option<&str>,
        tx: tokio::sync::mpsc::UnboundedSender<(String, bool)>,
    ) -> std::result::Result<i32, String> {
        let (stdout_read_fd, stdout_write_fd) = Self::create_pipe()?;
        let (stderr_read_fd, stderr_write_fd) = Self::create_pipe()?;

        let pid = unsafe { libc::fork() };

        if pid < 0 {
            unsafe {
                libc::close(stdout_read_fd);
                libc::close(stdout_write_fd);
                libc::close(stderr_read_fd);
                libc::close(stderr_write_fd);
            }
            return Err("fork() failed".to_string());
        }

        if pid == 0 {
            // ============ CHILD PROCESS ============
            unsafe {
                libc::close(stdout_read_fd);
                libc::close(stderr_read_fd);
                libc::dup2(stdout_write_fd, libc::STDOUT_FILENO);
                libc::dup2(stderr_write_fd, libc::STDERR_FILENO);
                libc::close(stdout_write_fd);
                libc::close(stderr_write_fd);
            }

            let result = Self::configure_and_start_vm(
                rootfs_path,
                cpus,
                memory_mb,
                command,
                args,
                workdir,
                env,
                network_scope,
                port_mappings,
                mounts,
                dns,
                gvproxy_socket,
            );

            if let Err(e) = result {
                eprintln!("libkrun VM start failed: {}", e);
            }
            unsafe { libc::_exit(1) };
        }

        // ============ PARENT PROCESS ============
        unsafe {
            libc::close(stdout_write_fd);
            libc::close(stderr_write_fd);
        }

        // Read stdout and stderr in separate threads, sending output through the channel
        let stdout_file = unsafe { std::fs::File::from_raw_fd(stdout_read_fd) };
        let stderr_file = unsafe { std::fs::File::from_raw_fd(stderr_read_fd) };

        let tx_stdout = tx.clone();
        let stdout_handle = std::thread::spawn(move || {
            let reader = BufReader::new(stdout_file);
            for line in reader.lines() {
                match line {
                    Ok(text) => {
                        let _ = tx_stdout.send((text, false));
                    }
                    Err(_) => break,
                }
            }
        });

        let tx_stderr = tx;
        let stderr_handle = std::thread::spawn(move || {
            let reader = BufReader::new(stderr_file);
            for line in reader.lines() {
                match line {
                    Ok(text) => {
                        let _ = tx_stderr.send((text, true));
                    }
                    Err(_) => break,
                }
            }
        });

        // Wait for reader threads to finish
        let _ = stdout_handle.join();
        let _ = stderr_handle.join();

        // Wait for child process
        let mut status: libc::c_int = 0;
        unsafe {
            libc::waitpid(pid, &mut status, 0);
        }

        let exit_code = if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else {
            -1
        };

        Ok(exit_code)
    }

    /// Configure a libkrun VM context and start it.
    /// This function is called in the forked child process.
    /// On success, krun_start_enter takes over and never returns.
    fn configure_and_start_vm(
        rootfs_path: &str,
        cpus: u32,
        memory_mb: u32,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
        _network_scope: &NetworkScope,
        port_mappings: &[(u16, u16)],
        mounts: &[(String, String)],
        dns: &[String],
        gvproxy_socket: Option<&str>,
    ) -> std::result::Result<(), String> {
        // Create VM context
        let ctx = ffi::create_ctx()?;

        // Configure VM resources
        let vcpus = if cpus > 255 { 255u8 } else { cpus as u8 };
        ffi::set_vm_config(ctx, vcpus, memory_mb)?;

        // Set rootfs
        ffi::set_root(ctx, rootfs_path)?;

        // Configure networking
        if let Some(socket_path) = gvproxy_socket {
            // gvproxy mode: configure virtio-net device connected to gvproxy's
            // unixgram socket. This automatically disables TSI networking.
            ffi::add_net_unixgram(
                ctx,
                socket_path,
                &ffi::GVPROXY_GUEST_MAC,
                ffi::COMPAT_NET_FEATURES,
                ffi::NET_FLAG_VFKIT,
            )?;
        }
        // When no net device is added, libkrun uses TSI networking (fallback).

        // Set port mappings (as a null-terminated array of "host:guest" strings)
        if !port_mappings.is_empty() {
            let mapping_strings: Vec<String> = port_mappings
                .iter()
                .map(|(hp, gp)| format!("{}:{}", hp, gp))
                .collect();
            ffi::set_port_map(ctx, Some(&mapping_strings))?;
        }

        // Add virtiofs mounts
        for (tag, path) in mounts {
            ffi::add_virtiofs(ctx, tag, path)?;
        }

        // Set working directory
        if let Some(wd) = workdir {
            ffi::set_workdir(ctx, wd)?;
        }

        // Build environment variables
        let mut env_vars: Vec<String> = env
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();

        // Add DNS configuration via environment if specified
        if !dns.is_empty() {
            env_vars.push(format!("NANOSANDBOX_DNS={}", dns.join(",")));
        }

        // Always set basic env vars
        if !env.contains_key("PATH") {
            env_vars.push("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string());
        }
        if !env.contains_key("HOME") {
            env_vars.push("HOME=/root".to_string());
        }
        if !env.contains_key("TERM") {
            env_vars.push("TERM=xterm-256color".to_string());
        }

        // Build the exec command.
        // When gvproxy is active AND the command is not agent-gateway (which does
        // its own network init), wrap in the nanosb-net-init script which
        // configures eth0 then execs the user's command.
        //
        // NOTE: libkrun's krun_set_exec doesn't support shebang scripts as PID 1,
        // and `/bin/sh -c` has a known parsing bug. We use `/bin/sh <script_path>`
        // which works correctly as the VM's init process.
        let (exec_path, exec_argv_owned): (&str, Vec<String>);

        let is_gateway = command.contains("agent-gateway");
        if gvproxy_socket.is_some() && !is_gateway {
            // Use the nanosb-net-init script to configure eth0 before running
            // the user's command. We pass it as a shell script file, not via -c.
            let init = "/usr/local/bin/nanosb-net-init";
            let mut v = vec![init.to_string(), command.to_string()];
            v.extend(args.iter().map(|a| a.to_string()));
            exec_path = init;
            exec_argv_owned = v;
        } else {
            // agent-gateway handles its own network init, or no gvproxy
            let mut v = vec![command.to_string()];
            v.extend(args.iter().map(|a| a.to_string()));
            exec_path = command;
            exec_argv_owned = v;
        }

        let exec_argv_refs: Vec<&str> = exec_argv_owned.iter().map(|s| s.as_str()).collect();

        // Set exec with explicit environment (don't inherit host env)
        ffi::set_exec(ctx, exec_path, &exec_argv_refs, Some(&env_vars))?;

        // Note: krun_start_enter takes over stdin/stdout/stderr of the current
        // process. Since we've dup2'd stdout/stderr to pipes in the parent,
        // VM output will naturally flow through those pipes.

        // Start the VM -- this never returns on success
        ffi::start_enter(ctx)
    }

    /// Create a Unix pipe, returning (read_fd, write_fd)
    fn create_pipe() -> std::result::Result<(i32, i32), String> {
        let mut fds = [0i32; 2];
        let ret = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if ret < 0 {
            Err("pipe() failed".to_string())
        } else {
            Ok((fds[0], fds[1]))
        }
    }

    /// Read all content from a file descriptor into a String
    fn read_fd(fd: i32) -> String {
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let mut reader = BufReader::new(file);
        let mut content = String::new();
        let _ = reader.read_to_string(&mut content);
        content
    }
}

// Public API matching the RuntimeBackend interface
impl LibkrunRuntime {
    /// Create a sandbox from a pre-prepared OCI bundle.
    ///
    /// The Sandbox orchestrator has already pulled the image and created the rootfs
    /// at `bundle_path/rootfs` using `ImageManager`. This method just stores the
    /// configuration for later use by `exec()` / `exec_stream()`.
    pub async fn create(
        &self,
        id: &str,
        config: &SandboxConfig,
        bundle_path: Option<&Path>,
    ) -> Result<()> {
        // Clean up any existing sandbox with this ID.
        // Extract the check result before any .await to avoid holding
        // MutexGuard (not Send) across an await point.
        let needs_cleanup = self.sandboxes.lock().unwrap().contains_key(id);
        if needs_cleanup {
            self.destroy(id).await?;
        }

        // Derive rootfs path from the bundle
        let bundle = bundle_path.ok_or_else(|| {
            Error::SandboxCreationFailed(
                "LibkrunRuntime requires a bundle path with rootfs (handles_image_pull=false)"
                    .to_string(),
            )
        })?;
        let rootfs_path = bundle.join("rootfs");

        if !rootfs_path.exists() {
            return Err(Error::SandboxCreationFailed(format!(
                "rootfs not found at {}",
                rootfs_path.display()
            )));
        }

        // Collect mount info
        let mounts: Vec<(String, String)> = config
            .mounts
            .iter()
            .filter(|m| m.mount_type == MountType::VirtioFs || m.mount_type == MountType::Bind)
            .enumerate()
            .map(|(i, m)| {
                let tag = format!("mount{}", i);
                let path = m.host_path.to_string_lossy().to_string();
                (tag, path)
            })
            .collect();

        // Collect port mappings
        let port_mappings: Vec<(u16, u16)> = config
            .network
            .port_mappings
            .iter()
            .map(|pm| (pm.host_port, pm.container_port))
            .collect();

        // DNS servers
        let dns = if !config.network.dns.is_empty() {
            config.network.dns.clone()
        } else if config.network.enabled {
            vec!["8.8.8.8".to_string(), "1.1.1.1".to_string()]
        } else {
            vec![]
        };

        // Start gvproxy for virtio-net networking if available
        let gvproxy = if config.network.enabled && GvproxyManager::is_available() {
            match GvproxyManager::start(id) {
                Ok(instance) => {
                    info!(
                        "gvproxy networking active for sandbox '{}' (socket: {})",
                        id,
                        instance.socket_path().display()
                    );
                    Some(instance)
                }
                Err(e) => {
                    warn!(
                        "Failed to start gvproxy for sandbox '{}': {} -- falling back to TSI",
                        id, e
                    );
                    None
                }
            }
        } else {
            if config.network.enabled && !GvproxyManager::is_available() {
                warn!(
                    "gvproxy not found -- using TSI networking for sandbox '{}' (outbound connections may be limited). \
                     Install gvproxy for full networking: https://github.com/containers/gvisor-tap-vsock/releases",
                    id
                );
            }
            None
        };

        let networking_mode = if gvproxy.is_some() { "gvproxy (virtio-net)" } else { "TSI (fallback)" };

        // Allow overriding the gateway binary for local development/testing.
        // Set NANOSANDBOX_GATEWAY_BIN=/path/to/agent-gateway to inject a local build.
        if let Ok(override_bin) = std::env::var("NANOSANDBOX_GATEWAY_BIN") {
            let gateway_dest = rootfs_path.join("usr/local/bin/agent-gateway");
            if let Err(e) = std::fs::copy(&override_bin, &gateway_dest) {
                warn!("Failed to inject gateway override from {}: {}", override_bin, e);
            } else {
                // Make executable
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&gateway_dest, std::fs::Permissions::from_mode(0o755));
                }
                info!("Injected gateway binary override from {}", override_bin);
            }
        }

        // Detect if the rootfs contains agent-gateway (persistent mode)
        let has_gateway = rootfs_path.join("usr/local/bin/agent-gateway").exists();
        if has_gateway {
            info!(
                "Detected agent-gateway in rootfs for sandbox '{}' -- persistent VM mode enabled",
                id
            );
        }

        let state = SandboxState {
            rootfs_path: rootfs_path.clone(),
            cpus: config.cpus,
            memory_mb: config.memory_mb,
            dns,
            network_scope: config.network.scope,
            port_mappings,
            mounts,
            gvproxy,
            gateway_port: None,
            vm_pid: None,
            has_gateway,
        };

        info!(
            "Created libkrun sandbox '{}' (rootfs: {}, networking: {}, persistent: {})",
            id,
            rootfs_path.display(),
            networking_mode,
            has_gateway
        );

        self.sandboxes.lock().unwrap().insert(id.to_string(), state);
        Ok(())
    }

    /// Start the sandbox.
    ///
    /// If the rootfs contains `/usr/local/bin/agent-gateway` (persistent mode),
    /// this boots a long-lived VM with agent-gateway as PID 1 and polls for
    /// health. Otherwise it is a no-op (ephemeral VMs are created per exec).
    pub async fn start(&self, id: &str) -> Result<()> {
        // Extract what we need while holding the lock briefly
        let has_gateway = {
            let sandboxes = self.sandboxes.lock().unwrap();
            let state = sandboxes
                .get(id)
                .ok_or_else(|| Error::SandboxNotFound(id.to_string()))?;
            state.has_gateway
        };

        if !has_gateway {
            // Ephemeral mode: nothing to start. VMs are created per exec.
            return Ok(());
        }

        // --- Persistent VM mode: boot the VM with agent-gateway as PID 1 ---

        // Find a free host port for the gateway
        let host_port = TcpListener::bind("127.0.0.1:0")
            .ok()
            .and_then(|l| l.local_addr().ok())
            .map(|a| a.port())
            .ok_or_else(|| {
                Error::SandboxCreationFailed("Failed to find free port for agent-gateway".into())
            })?;

        info!(
            "Starting persistent VM for sandbox '{}' (gateway port: {})",
            id, host_port
        );

        // Clone state for the blocking task (do NOT add gateway port mapping
        // here -- krun_set_port_map is a TSI feature that doesn't work with gvproxy.
        // Port forwarding is done via gvproxy's HTTP API after the VM boots.)
        let (rootfs_path, cpus, memory_mb, network_scope, port_mappings, mounts, dns, gvproxy_socket) = {
            let sandboxes = self.sandboxes.lock().unwrap();
            let state = sandboxes.get(id).unwrap();
            (
                state.rootfs_path.to_string_lossy().to_string(),
                state.cpus,
                state.memory_mb,
                state.network_scope,
                state.port_mappings.clone(),
                state.mounts.clone(),
                state.dns.clone(),
                state.gvproxy.as_ref().map(|g| g.socket_path().to_string_lossy().to_string()),
            )
        };

        // Boot the VM in a blocking task (fork + krun_start_enter)
        let id_owned = id.to_string();
        let vm_pid = tokio::task::spawn_blocking(move || {
            Self::boot_persistent_vm(
                &rootfs_path,
                cpus,
                memory_mb,
                &network_scope,
                &port_mappings,
                &mounts,
                &dns,
                gvproxy_socket.as_deref(),
            )
        })
        .await
        .map_err(|e| Error::ExecFailed(format!("Task join error: {}", e)))?
        .map_err(|e| Error::SandboxCreationFailed(format!("VM boot failed: {}", e)))?;

        // Expose gateway port via gvproxy's HTTP API (host_port -> guest:8080)
        {
            let sandboxes = self.sandboxes.lock().unwrap();
            if let Some(state) = sandboxes.get(id) {
                if let Some(ref gvproxy) = state.gvproxy {
                    gvproxy.expose_port(host_port, 8080).map_err(|e| {
                        Error::SandboxCreationFailed(format!(
                            "Failed to expose gateway port via gvproxy: {}",
                            e
                        ))
                    })?;
                    info!(
                        "Exposed gateway port via gvproxy: host:{} -> guest:8080",
                        host_port
                    );
                }
            }
        }

        // Store the VM PID and gateway port
        {
            let mut sandboxes = self.sandboxes.lock().unwrap();
            if let Some(state) = sandboxes.get_mut(&id_owned) {
                state.gateway_port = Some(host_port);
                state.vm_pid = Some(vm_pid);
            }
        }

        // Poll health endpoint until ready (timeout 30s)
        let health_url = format!("127.0.0.1:{}", host_port);
        let mut delay_ms = 100u64;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);

        loop {
            if std::time::Instant::now() > deadline {
                return Err(Error::Timeout(30));
            }

            match Self::http_get(&health_url, "/health") {
                Ok((200, _body)) => {
                    info!(
                        "Agent gateway healthy for sandbox '{}' at port {}",
                        id_owned, host_port
                    );
                    break;
                }
                Ok((code, body)) => {
                    debug!(
                        "Gateway not ready yet (HTTP {}): {}",
                        code, body.chars().take(100).collect::<String>()
                    );
                }
                Err(e) => {
                    debug!("Gateway not reachable yet: {}", e);
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            delay_ms = (delay_ms * 2).min(2000); // Exponential backoff, max 2s
        }

        Ok(())
    }

    /// Boot a persistent VM with agent-gateway as PID 1.
    /// Returns the child PID on success.
    fn boot_persistent_vm(
        rootfs_path: &str,
        cpus: u32,
        memory_mb: u32,
        network_scope: &NetworkScope,
        port_mappings: &[(u16, u16)],
        mounts: &[(String, String)],
        dns: &[String],
        gvproxy_socket: Option<&str>,
    ) -> std::result::Result<i32, String> {
        let pid = unsafe { libc::fork() };

        if pid < 0 {
            return Err("fork() failed".to_string());
        }

        if pid == 0 {
            // ============ CHILD PROCESS ============
            // Redirect stdout/stderr to /dev/null for the persistent VM
            // (output comes via HTTP/SSE, not pipes)
            let dev_null = unsafe { libc::open(b"/dev/null\0".as_ptr() as *const _, libc::O_WRONLY) };
            if dev_null >= 0 {
                unsafe {
                    libc::dup2(dev_null, libc::STDOUT_FILENO);
                    libc::dup2(dev_null, libc::STDERR_FILENO);
                    libc::close(dev_null);
                }
            }

            // Build env vars
            let mut env = HashMap::new();
            env.insert("PATH".to_string(), "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string());
            env.insert("HOME".to_string(), "/root".to_string());
            env.insert("TERM".to_string(), "dumb".to_string());

            // Add DNS config
            if !dns.is_empty() {
                env.insert("NANOSANDBOX_DNS".to_string(), dns.join(","));
            }

            let result = Self::configure_and_start_vm(
                rootfs_path,
                cpus,
                memory_mb,
                "/usr/local/bin/agent-gateway",
                &["agent-gateway"],
                Some("/workspace"),
                &env,
                network_scope,
                port_mappings,
                mounts,
                dns,
                gvproxy_socket,
            );

            if let Err(e) = result {
                // Write error to log file (stderr is redirected)
                let msg = format!("boot_persistent_vm: configure_and_start_vm failed: {}\n", e);
                let _ = std::io::Write::write_all(
                    &mut std::io::stderr(),
                    msg.as_bytes(),
                );
            }
            unsafe { libc::_exit(1) };
        }

        // ============ PARENT PROCESS ============
        Ok(pid)
    }

    /// Minimal HTTP GET using raw TcpStream. Returns (status_code, body).
    fn http_get(addr: &str, path: &str) -> std::result::Result<(u16, String), String> {
        let mut stream = std::net::TcpStream::connect(addr).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;

        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            path, addr
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|e| e.to_string())?;

        let mut reader = BufReader::new(stream);

        // Read status line
        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .map_err(|e| e.to_string())?;
        let status_code = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);

        // Skip headers
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).map_err(|e| e.to_string())?;
            if line.trim().is_empty() {
                break;
            }
        }

        // Read body
        let mut body = String::new();
        let _ = reader.read_to_string(&mut body);

        Ok((status_code, body))
    }

    /// HTTP POST that reads an SSE stream and calls on_output for each event.
    /// Returns the exit code from the final "exit" SSE event.
    ///
    /// Uses HTTP/1.0 with Connection: close to avoid chunked transfer encoding
    /// and ensure the server closes the connection when the response is complete.
    fn http_post_sse<F>(
        addr: &str,
        path: &str,
        json_body: &str,
        on_output: F,
    ) -> std::result::Result<i32, String>
    where
        F: Fn(&str, bool),
    {
        eprintln!("[http_post_sse] Connecting to {} path={}", addr, path);
        let mut stream = std::net::TcpStream::connect(addr).map_err(|e| {
            eprintln!("[http_post_sse] Connect failed: {}", e);
            e.to_string()
        })?;
        // No read timeout -- SSE streams can be very long-lived
        stream
            .set_read_timeout(None)
            .map_err(|e| e.to_string())?;

        // Use HTTP/1.0 + Connection: close to avoid chunked transfer encoding
        // and ensure the server closes the TCP connection after the response.
        let request = format!(
            "POST {} HTTP/1.0\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n{}",
            path, addr, json_body.len(), json_body
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|e| e.to_string())?;
        eprintln!("[http_post_sse] Request sent, reading response...");

        let mut reader = BufReader::new(stream);

        // Read HTTP status line
        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .map_err(|e| format!("Error reading status line: {}", e))?;
        let status_trimmed = status_line.trim();
        eprintln!("[http_post_sse] Status: {}", status_trimmed);

        // Check for non-200 status
        if !status_trimmed.contains("200") {
            // Read rest of response for error details
            let mut body = String::new();
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => body.push_str(&line),
                    Err(_) => break,
                }
            }
            return Err(format!(
                "HTTP error: {} body: {}",
                status_trimmed,
                body.trim()
            ));
        }

        // Skip remaining headers
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => return Err("Connection closed before headers finished".to_string()),
                Ok(_) => {
                    if line.trim().is_empty() {
                        break;
                    }
                }
                Err(e) => return Err(format!("Error reading headers: {}", e)),
            }
        }
        eprintln!("[http_post_sse] Headers done, reading SSE events...");

        // Read SSE events
        let mut exit_code = 0i32;
        let mut event_count = 0u32;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    eprintln!(
                        "[http_post_sse] EOF after {} events, exit_code={}",
                        event_count, exit_code
                    );
                    break; // EOF - connection closed
                }
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Some(data) = trimmed.strip_prefix("data: ") {
                        if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
                            match event.get("type").and_then(|v| v.as_str()) {
                                Some("stdout") => {
                                    event_count += 1;
                                    if let Some(text) = event.get("data").and_then(|v| v.as_str())
                                    {
                                        on_output(text, false);
                                    }
                                }
                                Some("stderr") => {
                                    event_count += 1;
                                    if let Some(text) = event.get("data").and_then(|v| v.as_str())
                                    {
                                        on_output(text, true);
                                    }
                                }
                                Some("exit") => {
                                    exit_code = event
                                        .get("code")
                                        .and_then(|v| v.as_i64())
                                        .unwrap_or(0)
                                        as i32;
                                    eprintln!(
                                        "[http_post_sse] Exit event: code={} after {} events",
                                        exit_code, event_count
                                    );
                                    break;
                                }
                                Some("error") => {
                                    if let Some(text) = event.get("data").and_then(|v| v.as_str())
                                    {
                                        eprintln!("[http_post_sse] Error event: {}", text);
                                        on_output(text, true);
                                    }
                                    exit_code = -1;
                                    break;
                                }
                                other => {
                                    eprintln!(
                                        "[http_post_sse] Unknown event type: {:?}",
                                        other
                                    );
                                }
                            }
                        } else {
                            eprintln!(
                                "[http_post_sse] Failed to parse JSON: {}",
                                &data[..data.len().min(100)]
                            );
                        }
                    }
                    // Non-data lines (e.g. chunk size lines from residual framing) are silently skipped
                }
                Err(e) => {
                    eprintln!("[http_post_sse] Read error: {}", e);
                    break;
                }
            }
        }

        eprintln!(
            "[http_post_sse] Done: exit_code={}, events={}",
            exit_code, event_count
        );
        Ok(exit_code)
    }

    /// Minimal HTTP POST (non-SSE). Returns (status_code, body).
    fn http_post(
        addr: &str,
        path: &str,
        json_body: &str,
    ) -> std::result::Result<(u16, String), String> {
        let mut stream = std::net::TcpStream::connect(addr).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .map_err(|e| e.to_string())?;

        let request = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            path, addr, json_body.len(), json_body
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|e| e.to_string())?;

        let mut reader = BufReader::new(stream);

        // Read status line
        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .map_err(|e| e.to_string())?;
        let status_code = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);

        // Skip headers
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).map_err(|e| e.to_string())?;
            if line.trim().is_empty() {
                break;
            }
        }

        // Read body
        let mut body = String::new();
        let _ = reader.read_to_string(&mut body);

        Ok((status_code, body))
    }

    /// Execute a command in the VM.
    ///
    /// In persistent mode (agent-gateway), sends an HTTP request to the gateway.
    /// In ephemeral mode, creates a fresh VM context each time.
    pub async fn exec(
        &self,
        id: &str,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
    ) -> Result<ExecOutput> {
        // Check if we're in persistent mode
        let gateway_port = {
            let sandboxes = self.sandboxes.lock().unwrap();
            let state = sandboxes
                .get(id)
                .ok_or_else(|| Error::SandboxNotFound(id.to_string()))?;
            state.gateway_port
        };

        if let Some(port) = gateway_port {
            // --- Persistent mode: HTTP to agent-gateway ---
            return self.exec_via_gateway(id, port, command, args, env).await;
        }

        // --- Ephemeral mode: fork + krun_start_enter ---
        // Clone the needed state while holding the lock briefly
        let (rootfs_path, cpus, memory_mb, network_scope, port_mappings, mounts, dns, gvproxy_socket) = {
            let sandboxes = self.sandboxes.lock().unwrap();
            let state = sandboxes
                .get(id)
                .ok_or_else(|| Error::SandboxNotFound(id.to_string()))?;
            (
                state.rootfs_path.to_string_lossy().to_string(),
                state.cpus,
                state.memory_mb,
                state.network_scope,
                state.port_mappings.clone(),
                state.mounts.clone(),
                state.dns.clone(),
                state.gvproxy.as_ref().map(|g| g.socket_path().to_string_lossy().to_string()),
            )
        };

        debug!(
            "Executing in libkrun VM '{}': {} {:?} (networking: {})",
            id, command, args,
            if gvproxy_socket.is_some() { "gvproxy" } else { "TSI" }
        );

        // Run the command in a forked child process with libkrun
        // Use spawn_blocking since fork+waitpid is blocking
        let command = command.to_string();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let workdir = workdir.map(|s| s.to_string());
        let env = env.clone();

        let result = tokio::task::spawn_blocking(move || {
            let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            Self::run_in_vm(
                &rootfs_path,
                cpus,
                memory_mb,
                &command,
                &args_refs,
                workdir.as_deref(),
                &env,
                &network_scope,
                &port_mappings,
                &mounts,
                &dns,
                gvproxy_socket.as_deref(),
            )
        })
        .await
        .map_err(|e| Error::ExecFailed(format!("Task join error: {}", e)))?
        .map_err(|e| Error::ExecFailed(e))?;

        Ok(ExecOutput {
            exit_code: result.0,
            stdout: result.1,
            stderr: result.2,
        })
    }

    /// Execute a command via the agent-gateway HTTP API (persistent mode).
    /// Buffers all output and returns it as ExecOutput.
    async fn exec_via_gateway(
        &self,
        id: &str,
        port: u16,
        command: &str,
        args: &[&str],
        env: &HashMap<String, String>,
    ) -> Result<ExecOutput> {
        let addr = format!("127.0.0.1:{}", port);
        let json_body = serde_json::json!({
            "command": command,
            "args": args,
            "env": env,
        })
        .to_string();

        debug!(
            "Executing via gateway for sandbox '{}': {} {:?}",
            id, command, args
        );

        let result = tokio::task::spawn_blocking(move || {
            let stdout = std::sync::Mutex::new(String::new());
            let stderr = std::sync::Mutex::new(String::new());
            let exit_code = Self::http_post_sse(
                &addr,
                "/api/v1/exec",
                &json_body,
                |text, is_stderr| {
                    if is_stderr {
                        let mut s = stderr.lock().unwrap();
                        if !s.is_empty() {
                            s.push('\n');
                        }
                        s.push_str(text);
                    } else {
                        let mut s = stdout.lock().unwrap();
                        if !s.is_empty() {
                            s.push('\n');
                        }
                        s.push_str(text);
                    }
                },
            )
            .map_err(|e| format!("Gateway exec failed: {}", e))?;

            let stdout = stdout.into_inner().unwrap();
            let stderr = stderr.into_inner().unwrap();
            Ok::<_, String>((exit_code, stdout, stderr))
        })
        .await
        .map_err(|e| Error::ExecFailed(format!("Task join error: {}", e)))?
        .map_err(|e| Error::ExecFailed(e))?;

        Ok(ExecOutput {
            exit_code: result.0,
            stdout: result.1,
            stderr: result.2,
        })
    }

    /// Execute with streaming output.
    ///
    /// In persistent mode (agent-gateway), sends an HTTP request to the gateway
    /// and streams SSE events back through the callback.
    /// In ephemeral mode, uses fork+exec with pipe-based streaming.
    pub async fn exec_stream<F>(
        &self,
        id: &str,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
        on_output: F,
    ) -> Result<i32>
    where
        F: Fn(&str, bool) + Send + Sync,
    {
        // Check if we're in persistent mode
        let gateway_port = {
            let sandboxes = self.sandboxes.lock().unwrap();
            let state = sandboxes
                .get(id)
                .ok_or_else(|| Error::SandboxNotFound(id.to_string()))?;
            state.gateway_port
        };

        if let Some(port) = gateway_port {
            // --- Persistent mode: stream via HTTP SSE from agent-gateway ---
            return self
                .exec_stream_via_gateway(id, port, command, args, env, on_output)
                .await;
        }

        // --- Ephemeral mode: fork + krun_start_enter with pipe streaming ---
        // Clone the needed state while holding the lock briefly
        let (rootfs_path, cpus, memory_mb, network_scope, port_mappings, mounts, dns, gvproxy_socket) = {
            let sandboxes = self.sandboxes.lock().unwrap();
            let state = sandboxes
                .get(id)
                .ok_or_else(|| Error::SandboxNotFound(id.to_string()))?;
            (
                state.rootfs_path.to_string_lossy().to_string(),
                state.cpus,
                state.memory_mb,
                state.network_scope,
                state.port_mappings.clone(),
                state.mounts.clone(),
                state.dns.clone(),
                state.gvproxy.as_ref().map(|g| g.socket_path().to_string_lossy().to_string()),
            )
        };

        debug!(
            "Executing (streaming) in libkrun VM '{}': {} {:?} (networking: {})",
            id, command, args,
            if gvproxy_socket.is_some() { "gvproxy" } else { "TSI" }
        );

        let command = command.to_string();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let workdir = workdir.map(|s| s.to_string());
        let env = env.clone();

        // Use a channel to bridge between the blocking fork and the async callback.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(String, bool)>();

        let handle = tokio::task::spawn_blocking(move || {
            let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            Self::run_in_vm_with_channel(
                &rootfs_path,
                cpus,
                memory_mb,
                &command,
                &args_refs,
                workdir.as_deref(),
                &env,
                &network_scope,
                &port_mappings,
                &mounts,
                &dns,
                gvproxy_socket.as_deref(),
                tx,
            )
        });

        // Stream output to the callback as it arrives
        while let Some((text, is_stderr)) = rx.recv().await {
            on_output(&text, is_stderr);
        }

        let result = handle
            .await
            .map_err(|e| Error::ExecFailed(format!("Task join error: {}", e)))?
            .map_err(|e| Error::ExecFailed(e))?;

        Ok(result)
    }

    /// Execute with streaming via the agent-gateway HTTP SSE API (persistent mode).
    async fn exec_stream_via_gateway<F>(
        &self,
        id: &str,
        port: u16,
        command: &str,
        args: &[&str],
        env: &HashMap<String, String>,
        on_output: F,
    ) -> Result<i32>
    where
        F: Fn(&str, bool) + Send + Sync,
    {
        let addr = format!("127.0.0.1:{}", port);
        let json_body = serde_json::json!({
            "command": command,
            "args": args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "env": env,
        })
        .to_string();

        debug!(
            "Streaming via gateway for sandbox '{}': {} {:?}",
            id, command, args
        );

        // Use a channel to bridge the blocking HTTP read with the async callback
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(String, bool)>();

        let handle = tokio::task::spawn_blocking(move || {
            Self::http_post_sse(&addr, "/api/v1/exec", &json_body, |text, is_stderr| {
                let _ = tx.send((text.to_string(), is_stderr));
            })
            .map_err(|e| format!("Gateway exec_stream failed: {}", e))
        });

        // Stream output to the callback as it arrives
        while let Some((text, is_stderr)) = rx.recv().await {
            on_output(&text, is_stderr);
        }

        let result = handle
            .await
            .map_err(|e| Error::ExecFailed(format!("Task join error: {}", e)))?
            .map_err(|e| Error::ExecFailed(e))?;

        Ok(result)
    }

    /// Stop the VM.
    ///
    /// In persistent mode, sends a stop request to the agent-gateway and waits
    /// for the VM process to exit. In ephemeral mode, this is a no-op.
    pub async fn stop(&self, id: &str) -> Result<()> {
        let (gateway_port, vm_pid) = {
            let sandboxes = self.sandboxes.lock().unwrap();
            let state = match sandboxes.get(id) {
                Some(s) => s,
                None => return Ok(()),
            };
            (state.gateway_port, state.vm_pid)
        };

        if let (Some(port), Some(pid)) = (gateway_port, vm_pid) {
            info!("Stopping persistent VM for sandbox '{}' (pid: {})", id, pid);

            // Send stop request to the gateway
            let addr = format!("127.0.0.1:{}", port);
            let _ = tokio::task::spawn_blocking(move || {
                let _ = Self::http_post(&addr, "/api/v1/stop", "{}");
            })
            .await;

            // Wait for the VM process to exit (with timeout)
            let pid_c = pid;
            let _ = tokio::task::spawn_blocking(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    let ret = unsafe { libc::waitpid(pid_c, std::ptr::null_mut(), libc::WNOHANG) };
                    if ret == pid_c || ret < 0 {
                        break; // Process exited or error
                    }
                    if std::time::Instant::now() > deadline {
                        // Force kill if still running
                        unsafe {
                            libc::kill(pid_c, libc::SIGKILL);
                            libc::waitpid(pid_c, std::ptr::null_mut(), 0);
                        }
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            })
            .await;

            // Clear the gateway state
            {
                let mut sandboxes = self.sandboxes.lock().unwrap();
                if let Some(state) = sandboxes.get_mut(id) {
                    state.gateway_port = None;
                    state.vm_pid = None;
                }
            }

            info!("Persistent VM stopped for sandbox '{}'", id);
        }

        Ok(())
    }

    /// Destroy the sandbox -- stops the VM, removes tracked state, and stops gvproxy.
    ///
    /// Rootfs lifecycle is managed by the Sandbox orchestrator via OciBundle,
    /// so we only need to remove our internal state entry and stop the gvproxy sidecar.
    pub async fn destroy(&self, id: &str) -> Result<()> {
        // Stop the persistent VM first (if running)
        self.stop(id).await?;

        let removed = self.sandboxes.lock().unwrap().remove(id);
        if let Some(mut state) = removed {
            // Kill the VM process if still running (safety net)
            if let Some(pid) = state.vm_pid {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                    libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG);
                }
            }

            // Stop gvproxy sidecar if running
            if let Some(ref mut gvproxy) = state.gvproxy {
                info!("Stopping gvproxy for sandbox '{}'", id);
                gvproxy.stop();
            }
            info!(
                "Destroyed libkrun sandbox '{}' (rootfs was: {})",
                id,
                state.rootfs_path.display()
            );
        } else {
            debug!("Sandbox '{}' not found for destruction (already cleaned up?)", id);
        }
        Ok(())
    }

    /// Send a structured agent message via the gateway's /api/v1/message endpoint.
    ///
    /// This is the primary API for multi-turn agent conversations in persistent mode.
    /// The gateway handles agent CLI spawning, session continuity (--continue, etc.),
    /// and streams output back as SSE events.
    ///
    /// Returns the exit code from the agent CLI.
    pub async fn send_message<F>(
        &self,
        id: &str,
        message: &str,
        agent: &str,
        model: &str,
        env: &HashMap<String, String>,
        on_output: F,
    ) -> Result<i32>
    where
        F: Fn(&str, bool) + Send + Sync,
    {
        let gateway_port = {
            let sandboxes = self.sandboxes.lock().unwrap();
            let state = sandboxes
                .get(id)
                .ok_or_else(|| Error::SandboxNotFound(id.to_string()))?;
            state.gateway_port.ok_or_else(|| {
                Error::ExecFailed(
                    "send_message requires a persistent VM with agent-gateway (no gateway_port set)"
                        .to_string(),
                )
            })?
        };

        let addr = format!("127.0.0.1:{}", gateway_port);
        let json_body = serde_json::json!({
            "message": message,
            "agent": agent,
            "model": model,
            "env": env,
        })
        .to_string();

        info!(
            "Sending message to agent '{}' in sandbox '{}' via gateway port {}",
            agent, id, gateway_port
        );
        // Log the message (truncated) and env keys for debugging
        let msg_preview: String = message.chars().take(200).collect();
        eprintln!(
            "[send_message] Starting: agent={}, model={}, gateway={}, msg_len={}, msg_preview={:?}, env_keys={:?}",
            agent, model, gateway_port, message.len(), msg_preview, env.keys().collect::<Vec<_>>()
        );

        // Use a channel to bridge the blocking HTTP read with the async callback
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(String, bool)>();

        let handle = tokio::task::spawn_blocking(move || {
            eprintln!("[send_message] spawn_blocking: starting http_post_sse");
            let result =
                Self::http_post_sse(&addr, "/api/v1/message", &json_body, |text, is_stderr| {
                    let _ = tx.send((text.to_string(), is_stderr));
                })
                .map_err(|e| format!("Gateway send_message failed: {}", e));
            eprintln!("[send_message] spawn_blocking: http_post_sse returned {:?}", result);
            result
        });

        // Stream output to the callback as it arrives
        let mut msg_count = 0u32;
        while let Some((text, is_stderr)) = rx.recv().await {
            msg_count += 1;
            let preview: String = text.chars().take(200).collect();
            eprintln!(
                "[send_message] Event #{} (stderr={}): {}",
                msg_count, is_stderr, preview
            );
            on_output(&text, is_stderr);
        }
        eprintln!(
            "[send_message] Channel closed after {} messages, awaiting join handle",
            msg_count
        );

        let result = handle
            .await
            .map_err(|e| Error::ExecFailed(format!("Task join error: {}", e)))?
            .map_err(|e| Error::ExecFailed(e))?;

        eprintln!("[send_message] Complete: exit_code={}", result);
        Ok(result)
    }

    /// Check if the sandbox is in persistent (gateway) mode.
    pub fn is_persistent(&self, id: &str) -> bool {
        self.sandboxes
            .lock()
            .unwrap()
            .get(id)
            .map(|s| s.has_gateway)
            .unwrap_or(false)
    }

    /// This runtime does NOT handle image pulling.
    ///
    /// The Sandbox orchestrator uses ImageManager (pure Rust) to pull the OCI image
    /// and create the rootfs, then passes the bundle_path to `create()`.
    pub fn handles_image_pull(&self) -> bool {
        false
    }
}

// Required for from_raw_fd
use std::os::unix::io::FromRawFd;
