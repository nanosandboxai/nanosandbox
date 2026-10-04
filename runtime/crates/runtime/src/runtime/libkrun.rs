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
use crate::config::{MountType, NetworkScope, SandboxConfig};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read as IoRead, Write as IoWrite};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;
use tracing::{debug, info, warn};

/// Configuration passed to the `internal-boot-vm` subprocess.
///
/// Serialized as JSON to stdin of the child process.
#[derive(Serialize, Deserialize)]
pub struct BootVmRequest {
    /// Sandbox identifier (for log file naming)
    pub sandbox_id: String,
    /// Path to the rootfs directory
    pub rootfs_path: String,
    /// Number of vCPUs
    pub cpus: u32,
    /// Memory in MiB
    pub memory_mb: u32,
    /// PID 1 command (e.g. "/bin/sh")
    pub command: String,
    /// PID 1 command arguments (e.g. ["-c", "while true; do sleep 3600; done"])
    pub command_args: Vec<String>,
    /// TSI network scope
    pub network_scope: NetworkScope,
    /// Port mappings (host, guest, protocol)
    pub port_mappings: Vec<(u16, u16, String)>,
    /// Mount points (host_path, container_path)
    pub mounts: Vec<(String, String)>,
    /// DNS servers
    pub dns: Vec<String>,
    /// Path to the gvproxy Unix socket (if gvproxy networking is active)
    pub gvproxy_socket: Option<String>,
    /// Run agent processes as root inside guest.
    #[serde(default)]
    pub run_as_root: bool,
}

/// Entry point for the `internal-boot-vm` subprocess.
///
/// Called by the nanosb binary when invoked with the hidden `internal-boot-vm`
/// argument. Reads a JSON [`BootVmRequest`] from stdin, configures a libkrun VM,
/// and calls `krun_start_enter` (which never returns on success).
///
/// This subprocess approach avoids a macOS Hypervisor.framework issue where
/// `hv_vm_create()` fails when called from a process that was `fork()`ed from
/// a multi-threaded parent (e.g. the TUI's tokio runtime). By using
/// `std::process::Command` (which uses `posix_spawn` on macOS), the child
/// process is clean and single-threaded.
pub fn handle_boot_vm_subprocess() -> ! {
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("internal-boot-vm: failed to read config from stdin: {}", e);
        std::process::exit(1);
    }

    let config: BootVmRequest = match serde_json::from_str(&input) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("internal-boot-vm: failed to parse config JSON: {}", e);
            std::process::exit(1);
        }
    };

    // macOS dlopen workaround: chdir to the directory containing libkrunfw
    preload_libkrunfw();

    // Enable libkrun's internal logging to stderr (captured by parent).
    // WARN level avoids the very verbose vCPU MMIO/interrupt traces from DEBUG.
    let _ = ffi::init_log(ffi::KRUN_LOG_TARGET_DEFAULT, ffi::KRUN_LOG_LEVEL_WARN);

    if config.run_as_root {
        std::env::set_var("NANOSB_RUN_AS_ROOT", "1");
    }

    // Build environment variables for the VM
    let mut env = HashMap::new();
    env.insert(
        "PATH".to_string(),
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
    );
    env.insert("HOME".to_string(), "/root".to_string());
    env.insert("TERM".to_string(), "dumb".to_string());
    if !config.dns.is_empty() {
        env.insert("NANOSANDBOX_DNS".to_string(), config.dns.join(","));
    }

    let args_refs: Vec<&str> = config.command_args.iter().map(|s| s.as_str()).collect();

    let result = LibkrunRuntime::configure_and_start_vm(
        &config.rootfs_path,
        config.cpus,
        config.memory_mb,
        &config.command,
        &args_refs,
        None,
        &env,
        &config.network_scope,
        &config.port_mappings,
        &config.mounts,
        &config.dns,
        config.gvproxy_socket.as_deref(),
    );

    if let Err(e) = result {
        eprintln!("internal-boot-vm: configure_and_start_vm failed: {}", e);
    }
    std::process::exit(1);
}

/// Pre-load `libkrunfw.5.dylib` using its full path so that libkrun's internal
/// `dlopen("libkrunfw.5.dylib")` (bare name) finds it already loaded.
///
/// On modern macOS, `dlopen` with a bare filename only searches `/usr/lib` and
/// the dyld cache — `/usr/local/lib` and `/opt/homebrew/lib` are NOT searched.
/// Entitled/codesigned binaries also have `DYLD_FALLBACK_LIBRARY_PATH` stripped.
/// Pre-loading with `RTLD_GLOBAL` makes the library available process-wide.
/// Ensure `libkrunfw.5.dylib` is discoverable by libkrun's internal `dlopen`.
///
/// On modern macOS, `dlopen("libkrunfw.5.dylib")` (bare name) only searches the
/// process's CWD, `/System/Volumes/Preboot/Cryptexes/OS`, and `/usr/lib`.
/// Entitled/codesigned binaries also have `DYLD_FALLBACK_LIBRARY_PATH` stripped.
///
/// The workaround: `chdir` to the directory containing the firmware dylib so that
/// libkrun's bare-name `dlopen` finds it in the CWD. This is safe because this
/// function is called in a forked child process that will be consumed by the VM.
#[cfg(target_os = "macos")]
fn preload_libkrunfw() {
    // Search ~/.nanosandbox/libs/ first (user-local install), then system dirs
    let mut search_dirs: Vec<String> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        search_dirs.push(format!("{}/.nanosandbox/libs", home));
    }
    search_dirs.extend(["/opt/homebrew/lib".to_string(), "/usr/local/lib".to_string()]);

    for dir in &search_dirs {
        let path = format!("{}/libkrunfw.5.dylib", dir);
        if std::path::Path::new(&path).exists() {
            if let Ok(cdir) = std::ffi::CString::new(dir.as_str()) {
                unsafe { libc::chdir(cdir.as_ptr()) };
            }
            return;
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn preload_libkrunfw() {
    // On Linux, dlopen search paths work normally.
}

/// Ensure the binary at `exe_path` has the `com.apple.security.hypervisor`
/// entitlement in its code signature. This is required for `hv_vm_create()`.
///
/// The binary may have lost its entitlement if cargo rebuilt it after the
/// initial `codesign-and-run.sh` signing. Re-signing here (which is safe
/// even while the binary is running) guarantees the NEXT process that
/// executes this binary will have the entitlement.
#[cfg(target_os = "macos")]
fn ensure_hypervisor_entitlement(exe_path: &Path) -> std::result::Result<(), String> {
    // Fast path: check if the binary already has the hypervisor entitlement.
    // This avoids the expensive (and race-prone) codesign operation when
    // the binary is already correctly signed.
    if has_hypervisor_entitlement(exe_path) {
        return Ok(());
    }

    const ENTITLEMENTS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.hypervisor</key>
    <true/>
</dict>
</plist>"#;

    // Write entitlements to a per-PID temp file to avoid races between
    // concurrent sandbox launches.
    let ent_path =
        std::env::temp_dir().join(format!("nanosb-entitlements-{}.plist", std::process::id()));
    std::fs::write(&ent_path, ENTITLEMENTS_XML)
        .map_err(|e| format!("Failed to write entitlements file: {}", e))?;

    // Retry once on failure (concurrent codesign on same binary can race)
    for attempt in 0..2u32 {
        let output = std::process::Command::new("codesign")
            .args(["--force", "--sign", "-", "--entitlements"])
            .arg(&ent_path)
            .arg(exe_path)
            .output()
            .map_err(|e| format!("codesign failed to execute: {}", e))?;

        if output.status.success() {
            let _ = std::fs::remove_file(&ent_path);
            return Ok(());
        }

        if attempt == 0 {
            // Brief pause before retry
            std::thread::sleep(std::time::Duration::from_millis(200));
            continue;
        }

        let _ = std::fs::remove_file(&ent_path);
        return Err(format!(
            "codesign failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(())
}

/// Check whether the binary already has the hypervisor entitlement.
#[cfg(target_os = "macos")]
fn has_hypervisor_entitlement(exe_path: &Path) -> bool {
    let output = std::process::Command::new("codesign")
        .args(["-d", "--entitlements", "-", "--xml"])
        .arg(exe_path)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.contains("com.apple.security.hypervisor")
        }
        _ => false,
    }
}

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
    /// Port mappings (host, guest, protocol)
    port_mappings: Vec<(u16, u16, String)>,
    /// Mounts to add as virtiofs
    mounts: Vec<(String, String)>,
    /// Custom DNS zone entries (name -> IP) for gvproxy
    dns_zones: HashMap<String, String>,
    /// gvproxy instance for virtio-net networking (None = TSI fallback)
    gvproxy: Option<GvproxyInstance>,
    /// PID of the forked child running the persistent VM
    vm_pid: Option<i32>,
    /// PID 1 command override (set by sandbox layer via config.command).
    command: Option<String>,
    /// PID 1 command arguments.
    command_args: Vec<String>,
    /// Run agent processes as root inside guest.
    run_as_root: bool,
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
    ///
    /// Fails naturally if libkrun is not loadable (dlopen will fail in ffi layer).
    /// Pre-flight validation (install hints, doctor commands) belongs in the CLI.
    pub async fn new() -> Result<Self> {
        // Set libkrun log level to Info
        if let Err(e) = ffi::set_log_level(3) {
            warn!("Failed to set libkrun log level: {}", e);
        }

        Ok(Self {
            sandboxes: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Acquire the sandboxes lock, recovering from poison if needed.
    ///
    /// If a thread panicked while holding the lock, we still access the
    /// inner data rather than permanently breaking the entire runtime.
    fn lock_sandboxes(&self) -> MutexGuard<'_, HashMap<String, SandboxState>> {
        self.sandboxes.lock().unwrap_or_else(|e| {
            warn!("Sandboxes mutex was poisoned, recovering: {}", e);
            e.into_inner()
        })
    }

    /// Configure a libkrun VM context and start it.
    /// This function is called in the forked child process.
    /// On success, krun_start_enter takes over and never returns.
    ///
    /// **Important**: Call `ffi::init_log()` BEFORE this function in the child
    /// process to enable libkrun's internal debug logging. This provides
    /// detailed error messages when `krun_start_enter` returns -EINVAL.
    #[allow(clippy::too_many_arguments)]
    fn configure_and_start_vm(
        rootfs_path: &str,
        cpus: u32,
        memory_mb: u32,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
        _network_scope: &NetworkScope,
        port_mappings: &[(u16, u16, String)],
        mounts: &[(String, String)],
        dns: &[String],
        gvproxy_socket: Option<&str>,
    ) -> std::result::Result<(), String> {
        // Create VM context
        let ctx = ffi::create_ctx()?;

        // Configure VM resources — clamp vCPUs to hypervisor max (kernel CONFIG_NR_CPUS).
        // libkrunfw defaults to CONFIG_NR_CPUS=8; query at runtime for the actual limit.
        let max_vcpus = ffi::get_max_vcpus().unwrap_or(8);
        let vcpus = cpus.min(max_vcpus).max(1) as u8;
        ffi::set_vm_config(ctx, vcpus, memory_mb)?;

        // Set rootfs
        ffi::set_root(ctx, rootfs_path)?;

        // Configure networking — use modern add_net_unixgram API (replaces deprecated set_gvproxy_path).
        if let Some(socket_path) = gvproxy_socket {
            ffi::add_net_unixgram(
                ctx,
                socket_path,
                &ffi::GVPROXY_GUEST_MAC,
                ffi::COMPAT_NET_FEATURES,
                ffi::NET_FLAG_VFKIT,
            )?;
        }
        // When no net device is added, libkrun uses TSI networking (fallback).

        // Set port mappings for TSI (as "host:guest" strings — TSI is TCP-only).
        // Only needed when gvproxy is NOT active, since gvproxy handles its own
        // port forwarding via the HTTP control API (expose_port).
        if gvproxy_socket.is_none() && !port_mappings.is_empty() {
            let mapping_strings: Vec<String> = port_mappings
                .iter()
                .map(|(hp, gp, _proto)| format!("{}:{}", hp, gp))
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
        let mut env_vars: Vec<String> = env.iter().map(|(k, v)| format!("{}={}", k, v)).collect();

        // Add DNS configuration via environment if specified
        if !dns.is_empty() && !env.contains_key("NANOSANDBOX_DNS") {
            env_vars.push(format!("NANOSANDBOX_DNS={}", dns.join(",")));
        }

        // Always set basic env vars
        if !env.contains_key("PATH") {
            env_vars.push(
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
            );
        }
        if !env.contains_key("HOME") {
            env_vars.push("HOME=/root".to_string());
        }
        if !env.contains_key("TERM") {
            env_vars.push("TERM=xterm-256color".to_string());
        }

        // Build the exec command.
        // IMPORTANT: Do NOT include exec_path as argv[0]. libkrun stores exec_path
        // separately as KRUN_INIT in the kernel cmdline, and init.c overrides
        // exec_argv[0] with KRUN_INIT. Any argv we pass here becomes argv[1..] in
        // the final execvp() call. Including exec_path as argv[0] would cause it
        // to appear as an extra argument (e.g., /bin/sh trying to read itself as
        // a script file).
        let exec_argv: Vec<&str> = args.iter().copied().collect();

        // Set exec with explicit environment (don't inherit host env)
        ffi::set_exec(ctx, command, &exec_argv, Some(&env_vars))?;

        // Remove config.json to prevent libkrun from reading stale commands.
        {
            let bundle_dir = std::path::Path::new(rootfs_path).parent();
            if let Some(dir) = bundle_dir {
                let config_path = dir.join("config.json");
                if config_path.exists() {
                    let _ = std::fs::remove_file(&config_path);
                }
            }
        }

        // Start the VM -- this never returns on success.
        // libkrun's internal logs (enabled via init_log in the caller) will
        // print the reason for any -EINVAL failure to stderr.
        ffi::start_enter(ctx)
    }
}

// Public API matching the RuntimeBackend interface
impl LibkrunRuntime {
    /// Dynamically forward a guest port to the same host port via gvproxy.
    pub fn guest_ip(&self, _id: &str) -> Option<String> {
        // On Linux/macOS, gvproxy forwards to localhost — no direct guest IP needed.
        None
    }

    pub fn is_vm_running(&self, id: &str) -> bool {
        // The VM runs in a child process (`internal-boot-vm`); check whether
        // its PID is still alive via kill(pid, 0). If vm_pid is None, the
        // sandbox hasn't reached spawn yet — treat as "still starting".
        let vm_pid = {
            let sandboxes = self.lock_sandboxes();
            match sandboxes.get(id) {
                Some(s) => s.vm_pid,
                None => return false,
            }
        };
        match vm_pid {
            None => true,
            Some(pid) => unsafe { libc::kill(pid, 0) == 0 },
        }
    }

    pub fn expose_port(&self, id: &str, port: u16) -> std::result::Result<(), String> {
        let sandboxes = self.lock_sandboxes();
        let state = sandboxes
            .get(id)
            .ok_or_else(|| "sandbox not found".to_string())?;
        let gvproxy = state
            .gvproxy
            .as_ref()
            .ok_or_else(|| "no gvproxy instance".to_string())?;
        gvproxy.expose_port(port, port, "tcp")
    }

    /// Create a sandbox from a pre-prepared OCI bundle.
    ///
    /// The Sandbox orchestrator has already pulled the image and created the rootfs
    /// at `bundle_path/rootfs` using `ImageManager`. This method just stores the
    /// configuration for later use by `start()`.
    pub async fn create(
        &self,
        id: &str,
        config: &SandboxConfig,
        bundle_path: Option<&Path>,
    ) -> Result<()> {
        // Clean up any existing sandbox with this ID.
        // Extract the check result before any .await to avoid holding
        // MutexGuard (not Send) across an await point.
        let needs_cleanup = self.lock_sandboxes().contains_key(id);
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

        // Collect mount info (canonicalize paths to prevent symlink escape / path traversal)
        let mounts: Vec<(String, String)> = config
            .mounts
            .iter()
            .filter(|m| m.mount_type == MountType::VirtioFs || m.mount_type == MountType::Bind)
            .enumerate()
            .map(|(i, m)| {
                let tag = format!("mount{}", i);
                let canonical = m.host_path.canonicalize().map_err(|e| {
                    Error::SandboxCreationFailed(format!(
                        "Mount path cannot be resolved: {}: {}",
                        m.host_path.display(),
                        e
                    ))
                })?;
                Ok((tag, canonical.to_string_lossy().to_string()))
            })
            .collect::<Result<Vec<_>>>()?;

        // Collect port mappings (preserving protocol for gvproxy forwarding)
        let port_mappings: Vec<(u16, u16, String)> = config
            .network
            .port_mappings
            .iter()
            .map(|pm| (pm.host_port, pm.container_port, pm.protocol.clone()))
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
                    // gvproxy failure causes fallback to TSI, which shares network namespace
                    // between VMs and causes port conflicts. This should be treated as an error.
                    return Err(Error::SandboxCreationFailed(format!(
                        "Failed to start gvproxy for sandbox '{}': {}. TSI fallback disabled to prevent port conflicts between sandboxes.",
                        id, e
                    )));
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

        let networking_mode = if gvproxy.is_some() {
            "gvproxy (virtio-net)"
        } else {
            "TSI (fallback)"
        };

        // Write virtiofs mount config for the guest init script.
        // The tags must match the "mount{i}" convention used in krun_add_virtiofs above.
        {
            let mut mount_lines = Vec::new();
            for (i, m) in config
                .mounts
                .iter()
                .filter(|m| m.mount_type == MountType::VirtioFs || m.mount_type == MountType::Bind)
                .enumerate()
            {
                if m.mount_type == MountType::VirtioFs {
                    mount_lines.push(format!("mount{} {}", i, m.container_path));
                }
            }
            if !mount_lines.is_empty() {
                let mount_config_path = rootfs_path.join("etc/nanosb-mounts");
                let _ = std::fs::write(&mount_config_path, mount_lines.join("\n") + "\n");
                debug!(
                    "Wrote virtiofs mount config ({} entries) to rootfs",
                    mount_lines.len()
                );
            }
        }

        let state = SandboxState {
            rootfs_path: rootfs_path.clone(),
            cpus: config.cpus,
            memory_mb: config.memory_mb,
            dns,
            network_scope: config.network.scope,
            port_mappings,
            mounts,
            dns_zones: config.network.dns_zones.clone(),
            gvproxy,
            vm_pid: None,
            command: config.command.clone(),
            command_args: config.command_args.clone(),
            run_as_root: config.run_as_root,
        };

        info!(
            "Created libkrun sandbox '{}' (rootfs: {}, networking: {})",
            id,
            rootfs_path.display(),
            networking_mode,
        );

        self.lock_sandboxes().insert(id.to_string(), state);
        Ok(())
    }

    /// Start the sandbox.
    ///
    /// Boots a long-lived VM subprocess. The PID 1 command is taken from
    /// `config.command` (set by the sandbox layer); falls back to sleep hold.
    pub async fn start(&self, id: &str) -> Result<()> {
        // PID 1 command: use config.command if set by the sandbox layer,
        // otherwise fall back to a sleep hold to keep the VM alive.
        let (pid1_command, pid1_args): (String, Vec<String>) = {
            let sandboxes = self.lock_sandboxes();
            let state = sandboxes
                .get(id)
                .ok_or_else(|| Error::SandboxNotFound(id.to_string()))?;
            if let Some(ref cmd) = state.command {
                info!("VM: PID 1 = {}", cmd);
                (cmd.clone(), state.command_args.clone())
            } else {
                info!("VM: no command set, using sleep hold");
                ("/bin/sleep".to_string(), vec!["infinity".to_string()])
            }
        };

        info!("Starting VM for sandbox '{}'", id);

        // Clone state for the blocking task
        let (
            rootfs_path,
            cpus,
            memory_mb,
            network_scope,
            port_mappings,
            mounts,
            dns,
            gvproxy_socket,
            run_as_root,
        ) = {
            let sandboxes = self.lock_sandboxes();
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
                state
                    .gvproxy
                    .as_ref()
                    .map(|g| g.socket_path().to_string_lossy().to_string()),
                state.run_as_root,
            )
        };


        // Boot the VM in a blocking task (subprocess + krun_start_enter)
        let id_owned = id.to_string();
        let pid1_args_refs: Vec<String> = pid1_args;
        let vm_pid = tokio::task::spawn_blocking(move || {
            let args_refs: Vec<&str> = pid1_args_refs.iter().map(|s| s.as_str()).collect();
            Self::boot_persistent_vm(
                &id_owned,
                &rootfs_path,
                cpus,
                memory_mb,
                &pid1_command,
                &args_refs,
                &network_scope,
                &port_mappings,
                &mounts,
                &dns,
                gvproxy_socket.as_deref(),
                run_as_root,
            )
        })
        .await
        .map_err(|e| Error::ExecFailed(format!("Task join error: {}", e)))?
        .map_err(|e| Error::SandboxCreationFailed(format!("VM boot failed: {}", e)))?;

        // Store VM PID
        {
            let mut sandboxes = self.lock_sandboxes();
            if let Some(state) = sandboxes.get_mut(id) {
                state.vm_pid = Some(vm_pid);
            }
        }

        // Verify gvproxy is still alive after boot
        let gvproxy_dead = {
            let mut sandboxes = self.lock_sandboxes();
            if let Some(state) = sandboxes.get_mut(id) {
                state.gvproxy.as_mut().is_some_and(|g| !g.is_alive())
            } else {
                false
            }
        };
        if gvproxy_dead {
            let _ = self.destroy(id).await;
            return Err(Error::SandboxCreationFailed(
                "gvproxy died during VM boot".to_string(),
            ));
        }

        // Expose user-configured port mappings and DNS zones via gvproxy
        {
            let sandboxes = self.lock_sandboxes();
            if let Some(state) = sandboxes.get(id) {
                if let Some(ref gvproxy) = state.gvproxy {
                    for (hp, gp, proto) in &state.port_mappings {
                        match gvproxy.expose_port(*hp, *gp, proto) {
                            Ok(()) => {
                                info!(
                                    "Exposed user port via gvproxy: host:{} -> guest:{} ({})",
                                    hp, gp, proto
                                );
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to expose user port via gvproxy: host:{} -> guest:{} ({}): {}",
                                    hp, gp, proto, e
                                );
                            }
                        }
                    }

                    for (name, ip) in &state.dns_zones {
                        match gvproxy.add_dns_zone(name, ip) {
                            Ok(()) => {
                                info!(
                                    "Configured DNS zone via gvproxy: {} -> {}",
                                    name, ip
                                );
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to configure DNS zone via gvproxy: {} -> {}: {}",
                                    name, ip, e
                                );
                            }
                        }
                    }
                }
            }
        }

        info!("VM started for sandbox '{}' (pid: {})", id, vm_pid);
        Ok(())
    }

    /// Boot a persistent VM subprocess.
    /// Returns the child PID on success.
    ///
    /// Uses `std::process::Command` (posix_spawn) instead of `fork()` to spawn
    /// the VM subprocess. This is critical on macOS: `hv_vm_create()` fails when
    /// called from a `fork()`ed child of a multi-threaded parent process (the
    /// TUI's tokio runtime). `posix_spawn` creates a clean, single-threaded
    /// child process where Hypervisor.framework works correctly.
    #[allow(clippy::too_many_arguments)]
    fn boot_persistent_vm(
        sandbox_id: &str,
        rootfs_path: &str,
        cpus: u32,
        memory_mb: u32,
        command: &str,
        command_args: &[&str],
        network_scope: &NetworkScope,
        port_mappings: &[(u16, u16, String)],
        mounts: &[(String, String)],
        dns: &[String],
        gvproxy_socket: Option<&str>,
        run_as_root: bool,
    ) -> std::result::Result<i32, String> {
        // Serialize VM configuration for the subprocess
        let request = BootVmRequest {
            sandbox_id: sandbox_id.to_string(),
            rootfs_path: rootfs_path.to_string(),
            cpus,
            memory_mb,
            command: command.to_string(),
            command_args: command_args.iter().map(|s| s.to_string()).collect(),
            network_scope: *network_scope,
            port_mappings: port_mappings.to_vec(),
            mounts: mounts.to_vec(),
            dns: dns.to_vec(),
            gvproxy_socket: gvproxy_socket.map(|s| s.to_string()),
            run_as_root,
        };

        let config_json = serde_json::to_string(&request)
            .map_err(|e| format!("Failed to serialize boot config: {}", e))?;

        // Use NANOSB_BINARY_PATH if set (for tests), otherwise use current_exe().
        // Tests run with the test binary, but need to spawn the actual nanosb binary.
        let exe_path = if let Ok(path_str) = std::env::var("NANOSB_BINARY_PATH") {
            PathBuf::from(path_str)
        } else {
            std::env::current_exe().map_err(|e| format!("Failed to get current exe path: {}", e))?
        };

        // Ensure the binary has the com.apple.security.hypervisor entitlement.
        // The binary may have lost its entitlement if cargo rebuilt it after the
        // initial codesign-and-run.sh signing. Re-signing here guarantees the
        // subprocess will have the entitlement when macOS evaluates it at exec.
        #[cfg(target_os = "macos")]
        ensure_hypervisor_entitlement(&exe_path)?;

        // Spawn the VM in a clean subprocess via posix_spawn (not fork)
        let mut cmd = std::process::Command::new(&exe_path);
        cmd.arg("internal-boot-vm")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // On Linux, libkrun dlopens libkrunfw.so.5 by bare name, but install-deps
        // places it at ~/.nanosandbox/libs/ which is not in ld.so's default
        // search path. Inject LD_LIBRARY_PATH so the child's dlopen finds it.
        // (macOS uses the chdir trick in preload_libkrunfw() instead.)
        #[cfg(target_os = "linux")]
        if let Ok(home) = std::env::var("HOME") {
            let nanosandbox_libs = format!("{}/.nanosandbox/libs", home);
            let new_path = match std::env::var("LD_LIBRARY_PATH") {
                Ok(existing) if !existing.is_empty() => {
                    format!("{}:{}", nanosandbox_libs, existing)
                }
                _ => nanosandbox_libs,
            };
            cmd.env("LD_LIBRARY_PATH", new_path);
        }

        // Create a new process group so the VM can be killed as a group
        // and doesn't become an orphan if the parent crashes.
        unsafe {
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn boot-vm subprocess: {}", e))?;

        // Write config JSON to child's stdin, then close it
        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| "Failed to open subprocess stdin".to_string())?;
            stdin
                .write_all(config_json.as_bytes())
                .map_err(|e| format!("Failed to write config to subprocess: {}", e))?;
            // stdin drops here, closing the pipe so child gets EOF
        }

        let pid = child.id() as i32;

        // Take stdout/stderr handles for background tracing forwarding.
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let prefix = sandbox_id.chars().take(8).collect::<String>();

        // Background threads: forward child stdout (VM console = agent output)
        // and stderr (libkrun warnings/errors) into the tracing subscriber.
        // The CLI is responsible for routing tracing events to log files; the
        // runtime itself does no file logging.
        std::thread::spawn(move || {
            let stdout_prefix = prefix.clone();
            let stdout_thread = stdout.map(|out| {
                std::thread::spawn(move || {
                    for line in BufReader::new(out).lines().flatten() {
                        info!("[vm-{}-stdout] {}", stdout_prefix, line);
                    }
                })
            });

            let stderr_prefix = prefix;
            let stderr_thread = stderr.map(|err| {
                std::thread::spawn(move || {
                    for line in BufReader::new(err).lines().flatten() {
                        info!("[vm-{}] {}", stderr_prefix, line);
                    }
                })
            });

            if let Some(t) = stdout_thread {
                let _ = t.join();
            }
            if let Some(t) = stderr_thread {
                let _ = t.join();
            }

            // child drops here. Child::drop does non-blocking waitpid
            // which is harmless — the real cleanup is via SIGKILL in destroy().
            drop(child);
        });

        Ok(pid)
    }


    /// Stop the VM by killing the VM process.
    pub async fn stop(&self, id: &str) -> Result<()> {
        let vm_pid = {
            let sandboxes = self.lock_sandboxes();
            let state = match sandboxes.get(id) {
                Some(s) => s,
                None => return Ok(()),
            };
            state.vm_pid
        };

        if let Some(pid) = vm_pid {
            info!("Stopping VM for sandbox '{}' (pid: {})", id, pid);

            // Kill the VM process group and wait for exit (with timeout)
            let pid_c = pid;
            let _ = tokio::task::spawn_blocking(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                // Send SIGTERM first for graceful shutdown
                unsafe { libc::kill(-pid_c, libc::SIGTERM) };
                loop {
                    let ret = unsafe { libc::waitpid(pid_c, std::ptr::null_mut(), libc::WNOHANG) };
                    if ret == pid_c || ret < 0 {
                        break; // Process exited or error
                    }
                    if std::time::Instant::now() > deadline {
                        // Force kill the process group if still running
                        unsafe {
                            libc::kill(-pid_c, libc::SIGKILL);
                            libc::waitpid(pid_c, std::ptr::null_mut(), 0);
                        }
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            })
            .await;

            // Clear the VM PID
            {
                let mut sandboxes = self.lock_sandboxes();
                if let Some(state) = sandboxes.get_mut(id) {
                    state.vm_pid = None;
                }
            }

            info!("VM stopped for sandbox '{}'", id);
        }

        Ok(())
    }

    /// Destroy the sandbox -- stops the VM, removes tracked state, and stops gvproxy.
    ///
    /// Rootfs lifecycle is managed by the Sandbox orchestrator via OciBundle,
    /// so we only need to remove our internal state entry and stop the gvproxy sidecar.
    pub async fn destroy(&self, id: &str) -> Result<()> {
        // Stop the VM first (if running)
        self.stop(id).await?;

        let removed = self.lock_sandboxes().remove(id);
        if let Some(mut state) = removed {
            // Kill the VM process group if still running (safety net)
            if let Some(pid) = state.vm_pid {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
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
            debug!(
                "Sandbox '{}' not found for destruction (already cleaned up?)",
                id
            );
        }
        Ok(())
    }

    /// This runtime does NOT handle image pulling.
    ///
    /// The Sandbox orchestrator uses ImageManager (pure Rust) to pull the OCI image
    /// and create the rootfs, then passes the bundle_path to `create()`.
    pub fn handles_image_pull(&self) -> bool {
        false
    }

}
