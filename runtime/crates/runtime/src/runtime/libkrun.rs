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
use crate::config::{ConsoleSpec, ExtraMount, MountType, NetworkScope, RuntimeMode, SandboxConfig};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read as IoRead, Write as IoWrite};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
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
    /// User to run the PID 1 command as (`UID`, `UID:GID`, or username).
    #[serde(default)]
    pub user: Option<String>,
    /// Home directory (`HOME`) to set for the guest command.
    #[serde(default)]
    pub home: Option<String>,
    /// Runtime mode: "legacy" or "next".
    #[serde(default = "default_runtime_mode")]
    pub runtime_mode: String,
    /// Console specification for next mode (stdin_fd, stdout_fd, stderr_fd, tty).
    #[serde(default)]
    pub console: Option<ConsoleSpec>,
    /// Extra virtiofs mounts for next mode (tag, target, readonly).
    #[serde(default)]
    pub extra_mounts: Vec<ExtraMount>,
    /// User environment variables passed into the guest (KEY -> VALUE).
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Host unix socket bridged to a guest vsock port (exec channel).
    #[serde(default)]
    pub vsock_socket: Option<String>,
    /// Guest vsock port the host socket is bridged to.
    #[serde(default)]
    pub vsock_port: Option<u32>,
}

fn default_runtime_mode() -> String {
    "legacy".to_string()
}

/// Look up a group name in the guest's `/etc/group`, returning its numeric gid.
fn lookup_group_gid(rootfs_path: &std::path::Path, name: &str) -> std::result::Result<String, String> {
    let path = rootfs_path.join("etc/group");
    let data = std::fs::read_to_string(&path)
        .map_err(|e| format!("read {}: {}", path.display(), e))?;
    for line in data.lines() {
        let mut fields = line.split(':');
        if fields.next() == Some(name) {
            let _passwd = fields.next();
            if let Some(gid) = fields.next() {
                if !gid.is_empty() {
                    return Ok(gid.to_string());
                }
            }
        }
    }
    Err(format!("group '{}' not found in guest /etc/group", name))
}

/// Resolve a user spec to numeric `(uid, gid, home)` against the guest rootfs.
///
/// The libkrun init blob requires **numeric** uid/gid, so usernames and group
/// names are resolved from the guest's `/etc/passwd` and `/etc/group`.
fn resolve_guest_user(
    rootfs_path: &std::path::Path,
    spec: &str,
    home_override: Option<&str>,
) -> std::result::Result<(String, Option<String>, Option<String>), String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("empty user spec".to_string());
    }
    let (user_part, group_part) = match spec.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (spec, None),
    };
    if user_part.is_empty() {
        return Err(format!("invalid user spec '{}'", spec));
    }

    let numeric = user_part.chars().all(|c| c.is_ascii_digit());
    let passwd_path = rootfs_path.join("etc/passwd");
    let passwd_data = std::fs::read_to_string(&passwd_path).unwrap_or_default();
    let entry_fields: Vec<&str> = if numeric {
        passwd_data
            .lines()
            .find(|l| l.split(':').nth(2) == Some(user_part))
            .map(|l| l.split(':').collect())
            .unwrap_or_default()
    } else {
        if passwd_data.is_empty() {
            return Err(format!(
                "cannot resolve user '{}': guest has no /etc/passwd (use a numeric uid, e.g. --user 1000)",
                user_part
            ));
        }
        let entry = passwd_data
            .lines()
            .find(|l| l.split(':').next() == Some(user_part))
            .ok_or_else(|| format!("user '{}' not found in guest /etc/passwd", user_part))?;
        entry.split(':').collect()
    };
    if !entry_fields.is_empty() && entry_fields.len() < 6 {
        return Err(format!("malformed passwd entry for '{}'", user_part));
    }

    let uid = if numeric {
        user_part.to_string()
    } else {
        entry_fields[2].to_string()
    };
    let default_gid = entry_fields.get(3).filter(|g| !g.is_empty()).map(|g| g.to_string());
    let passwd_home = entry_fields.get(5).filter(|h| !h.is_empty()).map(|h| h.to_string());

    let gid = match group_part {
        Some(g) if !g.is_empty() && g.chars().all(|c| c.is_ascii_digit()) => Some(g.to_string()),
        Some(g) if !g.is_empty() => Some(lookup_group_gid(rootfs_path, g)?),
        _ => default_gid,
    };

    let home = home_override
        .map(|s| s.to_string())
        .or(passwd_home)
        .filter(|h| !h.is_empty());

    Ok((uid, gid, home))
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

    // Build environment variables for the VM
    let mut env = HashMap::new();
    for (key, value) in &config.env {
        env.insert(key.clone(), value.clone());
    }
    if !env.contains_key("PATH") {
        env.insert(
            "PATH".to_string(),
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
        );
    }
    if !env.contains_key("HOME") {
        env.insert("HOME".to_string(), "/root".to_string());
    }
    if !env.contains_key("TERM") {
        env.insert("TERM".to_string(), "dumb".to_string());
    }
    if !config.dns.is_empty() && !env.contains_key("NANOSANDBOX_DNS") {
        env.insert("NANOSANDBOX_DNS".to_string(), config.dns.join(","));
    }

    let args_refs: Vec<&str> = config.command_args.iter().map(|s| s.as_str()).collect();

    let result = match config.runtime_mode.as_str() {
        "next" => LibkrunRuntime::configure_and_start_vm_next(
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
            config.user.as_deref(),
            config.home.as_deref(),
            config.console.as_ref(),
            &config.extra_mounts,
            config.vsock_socket.as_deref(),
            config.vsock_port,
        ),
        _ => LibkrunRuntime::configure_and_start_vm(
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
        ),
    };

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
    /// User environment variables passed into the guest.
    env: HashMap<String, String>,
    /// User to run the PID 1 command as (`UID`, `UID:GID`, or username).
    user: Option<String>,
    /// Home directory (`HOME`) for the guest command.
    home: Option<String>,
    /// Host unix socket bridged to a guest vsock port (exec channel).
    vsock_socket: Option<String>,
    /// Guest vsock port for the exec channel.
    vsock_port: Option<u32>,
    /// VM subprocess exit code (set when the VM exits).
    exit_code: Arc<Mutex<Option<i32>>>,
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

    /// Next-mode VM configuration: console I/O, extra virtiofs mounts, vanilla entrypoint.
    ///
    /// This is the zero-image-customization path. It:
    /// 1. Disables the implicit console (so we can wire our own)
    /// 2. Adds a virtio-console with the provided host fds
    /// 3. Registers extra virtiofs mounts (guest-side mounting via init patch)
    /// 4. Sets the exec command from the image entrypoint (no wrapper)
    /// 5. Starts the VM (never returns on success)
    #[allow(clippy::too_many_arguments)]
    fn configure_and_start_vm_next(
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
        user: Option<&str>,
        home: Option<&str>,
        console: Option<&ConsoleSpec>,
        extra_mounts: &[ExtraMount],
        vsock_socket: Option<&str>,
        vsock_port: Option<u32>,
    ) -> std::result::Result<(), String> {
        // Create VM context
        let ctx = ffi::create_ctx()?;

        // Configure VM resources
        let max_vcpus = ffi::get_max_vcpus().unwrap_or(8);
        let vcpus = cpus.min(max_vcpus).max(1) as u8;
        ffi::set_vm_config(ctx, vcpus, memory_mb)?;

        // Set rootfs
        ffi::set_root(ctx, rootfs_path)?;

        // Configure networking
        if let Some(socket_path) = gvproxy_socket {
            ffi::add_net_unixgram(
                ctx,
                socket_path,
                &ffi::GVPROXY_GUEST_MAC,
                ffi::COMPAT_NET_FEATURES,
                ffi::NET_FLAG_VFKIT,
            )?;
        }
        if gvproxy_socket.is_none() && !port_mappings.is_empty() {
            let mapping_strings: Vec<String> = port_mappings
                .iter()
                .map(|(hp, gp, _proto)| format!("{}:{}", hp, gp))
                .collect();
            ffi::set_port_map(ctx, Some(&mapping_strings))?;
        }

        // Add standard virtiofs mounts (host-side registration)
        for (tag, path) in mounts {
            ffi::add_virtiofs(ctx, tag, path)?;
        }

        // Add extra virtiofs mounts (next mode: tag-based, guest-side mounting via init patch)
        for em in extra_mounts {
            // Register the tag with the host path so libkrun shares the correct
            // host directory. The init patch mounts it at `em.target` inside the guest.
            ffi::add_virtiofs(ctx, &em.tag, &em.host_path)?;
        }

        // Dedicated host↔guest exec channel: bridge a host unix socket to a
        // guest vsock port. Independent of the network stack (works with
        // networking disabled), matching the microsandbox/Modal model.
        // vsock must be enabled (`krun_add_vsock`) before adding a port, else
        // `krun_add_vsock_port` returns ENODEV.
        if let (Some(sock), Some(port)) = (vsock_socket, vsock_port) {
            let _ = std::fs::remove_file(sock);
            // Enable the vsock device. This libkrun build (v1.19.5) defaults
            // vsock to Implicit (already enabled), so `add_vsock` may return
            // EEXIST (-17) — that is benign, not a failure. Then register the
            // port with `listen=true`: libkrun listens on the host socket and
            // the guest dials out to CID 2.
            match ffi::add_vsock(ctx, 0) {
                Ok(()) => {}
                Err(e) if e.contains("-17") || e.contains("EEXIST") => {}
                Err(e) => return Err(format!("add_vsock: {}", e)),
            }
            ffi::add_vsock_port2(ctx, port, sock, true)?;
        }

        // Set working directory
        if let Some(wd) = workdir {
            ffi::set_workdir(ctx, wd)?;
        }

        // Build environment variables
        let mut env_vars: Vec<String> = env.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
        if !dns.is_empty() && !env.contains_key("NANOSANDBOX_DNS") {
            env_vars.push(format!("NANOSANDBOX_DNS={}", dns.join(",")));
        }
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

        // Next-mode env vars for the patched init (see runtime/scripts/patches/README.md)
        env_vars.push("KRUN_NEXT_MODE=1".to_string());

        // Serialize extra mounts for the init patch
        if !extra_mounts.is_empty() {
            let mounts_json: Vec<serde_json::Value> = extra_mounts
                .iter()
                .map(|em| {
                    serde_json::json!({
                        "tag": em.tag,
                        "target": em.target,
                        "readonly": em.readonly,
                    })
                })
                .collect();
            env_vars.push(format!(
                "KRUN_EXTRA_MOUNTS={}",
                serde_json::to_string(&mounts_json).unwrap_or_default()
            ));
        }

        // Serialize network config for the init patch
        let dns_list: Vec<&str> = if !dns.is_empty() {
            dns.iter().map(|s| s.as_str()).collect()
        } else {
            vec!["8.8.8.8", "1.1.1.1"]
        };
        let net_config = serde_json::json!({
            "ip": "192.168.127.2/24",
            "gateway": "192.168.127.1",
            "dns": dns_list,
        });
        env_vars.push(format!(
            "KRUN_NET_CONFIG={}",
            serde_json::to_string(&net_config).unwrap_or_default()
        ));

        // Guest privilege drop: the init blob reads `uid`/`gid` from the config
        // file named by `KRUN_CONFIG` and applies setgid/setuid before exec.
        // `KRUN_HOME` is applied by init so the dropped-privilege process finds
        // the agent config mounted under its home directory.
        if let Some(ref user) = user {
            let (uid, gid, resolved_home) =
                resolve_guest_user(std::path::Path::new(rootfs_path), user, home)?;
            let guest_config = "/.nanosb_user.json";
            let host_config = std::path::Path::new(rootfs_path).join(".nanosb_user.json");
            let doc = match gid {
                Some(ref g) => serde_json::json!({ "uid": uid, "gid": g }),
                None => serde_json::json!({ "uid": uid }),
            };
            std::fs::write(&host_config, serde_json::to_string(&doc).unwrap_or_default())
                .map_err(|e| format!("failed to write {}: {}", host_config.display(), e))?;
            env_vars.push(format!("KRUN_CONFIG={}", guest_config));

            let name_part = user.split(':').next().unwrap_or(user);
            if !name_part.is_empty() && !name_part.chars().all(|c| c.is_ascii_digit()) {
                env_vars.push(format!("USER={}", name_part));
                env_vars.push(format!("LOGNAME={}", name_part));
            }
            if let Some(h) = resolved_home {
                if !h.starts_with('/') {
                    return Err(format!(
                        "invalid home '{}': must be an absolute guest path",
                        h
                    ));
                }
                // Materialize the home directory inside the guest rootfs so the
                // dropped-privilege process (and its config mounts) has a target.
                let host_home = std::path::Path::new(rootfs_path).join(h.trim_start_matches('/'));
                if !host_home.exists() {
                    std::fs::create_dir_all(&host_home)
                        .map_err(|e| format!("failed to create home {}: {}", host_home.display(), e))?;
                }
                env_vars.push(format!("KRUN_HOME={}", h));
            }
        }

        // Set up console I/O (next mode)
        // Disable the implicit console first, then wire our own virtio-console.
        ffi::disable_implicit_console(ctx)?;

        if let Some(cons) = console {
            if cons.tty {
                // TTY mode: use multiport console with a TTY port.
                // This enables raw-mode TUI, SIGWINCH resize, etc.
                let console_id = ffi::add_virtio_console_multiport(ctx)?;
                ffi::add_console_port_tty(ctx, console_id, "krun-console", cons.stdin_fd)?;
                // In TTY mode, stdout/stderr go through the TTY port.
                // We still set up the default console for output as a fallback.
                ffi::add_virtio_console_default(ctx, -1, cons.stdout_fd, cons.stderr_fd)?;
            } else {
                // Raw fd mode: wire stdin/stdout/stderr directly.
                ffi::add_virtio_console_default(ctx, cons.stdin_fd, cons.stdout_fd, cons.stderr_fd)?;
            }
        } else {
            // No console spec: use default console with inherited fds.
            ffi::add_virtio_console_default(ctx, 0, 1, 2)?;
        }

        // Build exec argv (vanilla entrypoint — no wrapper)
        let exec_argv: Vec<&str> = args.iter().copied().collect();

        // Set exec with explicit environment
        ffi::set_exec(ctx, command, &exec_argv, Some(&env_vars))?;

        // Remove config.json to prevent libkrun from reading stale commands
        {
            let bundle_dir = std::path::Path::new(rootfs_path).parent();
            if let Some(dir) = bundle_dir {
                let config_path = dir.join("config.json");
                if config_path.exists() {
                    let _ = std::fs::remove_file(&config_path);
                }
            }
        }

        // Start the VM — never returns on success
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

    pub fn vm_pid(&self, id: &str) -> Option<i32> {
        self.lock_sandboxes().get(id).and_then(|s| s.vm_pid)
    }

    pub fn vm_exit_code(&self, id: &str) -> Option<i32> {
        let slot = self.lock_sandboxes().get(id).map(|s| s.exit_code.clone())?;
        slot.lock().ok().and_then(|v| *v)
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

        // Write virtiofs mount config for the guest init script (legacy mode only).
        // In next mode, the libkrun init patch handles guest-side mounting.
        if config.runtime_mode != RuntimeMode::Next {
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

        if config.run_as_root {
            warn!(
                "sandbox '{}': `run_as_root` is deprecated (equivalent to user = \"0\"); use `user` instead",
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
            dns_zones: config.network.dns_zones.clone(),
            gvproxy,
            vm_pid: None,
            command: config.command.clone(),
            command_args: config.command_args.clone(),
            env: config.env.clone(),
            user: config
                .user
                .clone()
                .or_else(|| config.run_as_root.then(|| "0".to_string())),
            home: config.home.clone(),
            vsock_socket: config.vsock_socket.clone(),
            vsock_port: config.vsock_port,
            exit_code: Arc::new(Mutex::new(None)),
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
            env,
            user,
            home,
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
                state.env.clone(),
                state.user.clone(),
                state.home.clone(),
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
                &env,
                user.as_deref(),
                home.as_deref(),
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

    /// Boot a persistent VM subprocess (next mode).
    /// Returns the child PID on success.
    ///
    fn dup_console_fd(fd: i32) -> std::result::Result<i32, String> {
        if fd < 0 {
            return Ok(fd);
        }
        let new_fd = unsafe { libc::fcntl(fd, libc::F_DUPFD, 3) };
        if new_fd < 0 {
            return Err(format!(
                "Failed to duplicate console fd {}: {}",
                fd,
                std::io::Error::last_os_error()
            ));
        }
        Ok(new_fd)
    }

    /// Like `boot_persistent_vm` but accepts console fds and extra mounts
    /// for the next-mode (zero-image-customization) path. The console fds
    /// are inherited by the subprocess via `Stdio::from_raw_fd`.
    #[allow(clippy::too_many_arguments)]
    fn boot_persistent_vm_next(
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
        user: Option<&str>,
        home: Option<&str>,
        env: &HashMap<String, String>,
        console: Option<ConsoleSpec>,
        extra_mounts: Vec<ExtraMount>,
        vsock_socket: Option<&str>,
        vsock_port: Option<u32>,
        exit_code_slot: Arc<Mutex<Option<i32>>>,
    ) -> std::result::Result<i32, String> {
        let mut request = BootVmRequest {
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
            run_as_root: false,
            user: user.map(|s| s.to_string()),
            home: home.map(|s| s.to_string()),
            runtime_mode: "next".to_string(),
            console,
            extra_mounts,
            env: env.clone(),
            vsock_socket: vsock_socket.map(|s| s.to_string()),
            vsock_port,
        };

        let mut console_dups: Vec<i32> = Vec::new();
        if let Some(cons) = request.console.as_mut() {
            cons.stdin_fd = Self::dup_console_fd(cons.stdin_fd)?;
            cons.stdout_fd = Self::dup_console_fd(cons.stdout_fd)?;
            cons.stderr_fd = Self::dup_console_fd(cons.stderr_fd)?;
            for fd in [cons.stdin_fd, cons.stdout_fd, cons.stderr_fd] {
                if fd >= 0 {
                    console_dups.push(fd);
                }
            }
        }

        let config_json = serde_json::to_string(&request)
            .map_err(|e| format!("Failed to serialize boot config: {}", e))?;

        let exe_path = if let Ok(path_str) = std::env::var("NANOSB_BINARY_PATH") {
            PathBuf::from(path_str)
        } else {
            std::env::current_exe().map_err(|e| format!("Failed to get current exe path: {}", e))?
        };

        #[cfg(target_os = "macos")]
        ensure_hypervisor_entitlement(&exe_path)?;

        let mut cmd = std::process::Command::new(&exe_path);
        cmd.arg("internal-boot-vm")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

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

        unsafe {
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn boot-vm subprocess: {}", e))?;

        for fd in console_dups {
            unsafe {
                libc::close(fd);
            }
        }

        // Write config JSON to child's stdin, then close it
        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| "Failed to open subprocess stdin".to_string())?;
            stdin
                .write_all(config_json.as_bytes())
                .map_err(|e| format!("Failed to write config to subprocess: {}", e))?;
        }

        let pid = child.id() as i32;

        // Take stdout/stderr handles for background tracing forwarding.
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let prefix = sandbox_id.chars().take(8).collect::<String>();

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

            if let Ok(status) = child.wait() {
                if let Some(code) = status.code() {
                    if let Ok(mut slot) = exit_code_slot.lock() {
                        *slot = Some(code);
                    }
                }
            }
        });

        Ok(pid)
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
        env: &HashMap<String, String>,
        user: Option<&str>,
        home: Option<&str>,
    ) -> std::result::Result<i32, String> {
        // Serialize VM configuration for the subprocess
        // Determine runtime mode from environment (set by the supervisor).
        // Default is "legacy" for backward compatibility.
        let runtime_mode = std::env::var("NANOSB_RUNTIME").unwrap_or_else(|_| "legacy".to_string());

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
            run_as_root: false,
            user: user.map(|s| s.to_string()),
            home: home.map(|s| s.to_string()),
            runtime_mode,
            console: None,   // Set by the supervisor via env or config
            extra_mounts: Vec::new(), // Set by the supervisor via config
            env: env.clone(),
            vsock_socket: None,
            vsock_port: None,
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


    /// Start the sandbox in next mode with console fds and extra mounts.
    ///
    /// This is the zero-image-customization path. The caller (supervisor) provides
    /// the console fds and extra mount specs. The VM subprocess inherits the fds.
    pub async fn start_next(
        &self,
        id: &str,
        console: Option<ConsoleSpec>,
        extra_mounts: Vec<ExtraMount>,
    ) -> Result<()> {
        let (pid1_command, pid1_args): (String, Vec<String>) = {
            let sandboxes = self.lock_sandboxes();
            let state = sandboxes
                .get(id)
                .ok_or_else(|| Error::SandboxNotFound(id.to_string()))?;
            if let Some(ref cmd) = state.command {
                info!("VM (next): PID 1 = {}", cmd);
                (cmd.clone(), state.command_args.clone())
            } else {
                info!("VM (next): no command set, using sleep hold");
                ("/bin/sleep".to_string(), vec!["infinity".to_string()])
            }
        };

        info!("Starting VM (next mode) for sandbox '{}'", id);

        let (
            rootfs_path,
            cpus,
            memory_mb,
            network_scope,
            port_mappings,
            mounts,
            dns,
            gvproxy_socket,
            env,
            user,
            home,
            vsock_socket,
            vsock_port,
            exit_code_slot,
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
                state.env.clone(),
                state.user.clone(),
                state.home.clone(),
                state.vsock_socket.clone(),
                state.vsock_port,
                state.exit_code.clone(),
            )
        };

        let id_owned = id.to_string();
        let pid1_args_refs: Vec<String> = pid1_args;
        let vm_pid = tokio::task::spawn_blocking(move || {
            let args_refs: Vec<&str> = pid1_args_refs.iter().map(|s| s.as_str()).collect();
            Self::boot_persistent_vm_next(
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
                user.as_deref(),
                home.as_deref(),
                &env,
                console,
                extra_mounts,
                vsock_socket.as_deref(),
                vsock_port,
                exit_code_slot,
            )
        })
        .await
        .map_err(|e| Error::ExecFailed(format!("Task join error: {}", e)))?
        .map_err(|e| Error::SandboxCreationFailed(format!("VM boot failed: {}", e)))?;

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

        info!("VM (next mode) started for sandbox '{}' (pid: {})", id, vm_pid);
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_rootfs(passwd: &str, group: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nanosb-usr-{}-{}", std::process::id(), unique_nonce()));
        let etc = dir.join("etc");
        std::fs::create_dir_all(&etc).unwrap();
        std::fs::write(etc.join("passwd"), passwd).unwrap();
        std::fs::write(etc.join("group"), group).unwrap();
        dir
    }

    fn unique_nonce() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    }

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/sh\ndeveloper:x:1000:1000:Dev:/home/developer:/bin/bash\n";
    const GROUP: &str = "root:x:0:\ndeveloper:x:1000:\nwheel:x:10:\n";

    #[test]
    fn resolve_by_name() {
        let root = fixture_rootfs(PASSWD, GROUP);
        let (uid, gid, home) = resolve_guest_user(&root, "developer", None).unwrap();
        assert_eq!(uid, "1000");
        assert_eq!(gid.as_deref(), Some("1000"));
        assert_eq!(home.as_deref(), Some("/home/developer"));
    }

    #[test]
    fn resolve_numeric_uid_backfills_from_passwd() {
        let root = fixture_rootfs(PASSWD, GROUP);
        let (uid, gid, home) = resolve_guest_user(&root, "1000", None).unwrap();
        assert_eq!(uid, "1000");
        assert_eq!(gid.as_deref(), Some("1000"));
        assert_eq!(home.as_deref(), Some("/home/developer"));
    }

    #[test]
    fn resolve_uid_colon_gid_numeric() {
        let root = fixture_rootfs(PASSWD, GROUP);
        let (uid, gid, _) = resolve_guest_user(&root, "1000:2000", None).unwrap();
        assert_eq!(uid, "1000");
        assert_eq!(gid.as_deref(), Some("2000"));
    }

    #[test]
    fn resolve_name_colon_groupname() {
        let root = fixture_rootfs(PASSWD, GROUP);
        let (uid, gid, home) = resolve_guest_user(&root, "developer:wheel", None).unwrap();
        assert_eq!(uid, "1000");
        assert_eq!(gid.as_deref(), Some("10"));
        assert_eq!(home.as_deref(), Some("/home/developer"));
    }

    #[test]
    fn home_override_wins() {
        let root = fixture_rootfs(PASSWD, GROUP);
        let (_, _, home) = resolve_guest_user(&root, "developer", Some("/custom")).unwrap();
        assert_eq!(home.as_deref(), Some("/custom"));
    }

    #[test]
    fn unknown_name_errors() {
        let root = fixture_rootfs(PASSWD, GROUP);
        let err = resolve_guest_user(&root, "nobody", None).unwrap_err();
        assert!(err.contains("not found"), "{}", err);
    }

    #[test]
    fn empty_spec_errors() {
        let root = fixture_rootfs(PASSWD, GROUP);
        assert!(resolve_guest_user(&root, "  ", None).is_err());
    }

    #[test]
    fn name_without_passwd_gives_actionable_error() {
        let dir = std::env::temp_dir().join(format!("nanosb-nopasswd-{}-{}", std::process::id(), unique_nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = resolve_guest_user(&dir, "developer", None).unwrap_err();
        assert!(err.contains("/etc/passwd"), "{}", err);
        assert!(err.contains("numeric uid"), "{}", err);
    }

    #[test]
    fn numeric_uid_without_passwd_still_works() {
        let dir = std::env::temp_dir().join(format!("nanosb-nopasswd2-{}-{}", std::process::id(), unique_nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        let (uid, gid, home) = resolve_guest_user(&dir, "1000", None).unwrap();
        assert_eq!(uid, "1000");
        assert_eq!(gid, None);
        assert_eq!(home, None);
    }
}
