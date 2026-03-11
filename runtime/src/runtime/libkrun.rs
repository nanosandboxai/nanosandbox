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
use crate::config::{McpServerConfig, MountType, NetworkScope, SandboxConfig};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read as IoRead, Write as IoWrite};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
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
    /// PID 1 command arguments (e.g. ["sh", "/usr/local/bin/nanosb-init.sh"])
    pub command_args: Vec<String>,
    /// TSI network scope
    pub network_scope: NetworkScope,
    /// Port mappings (host, guest)
    pub port_mappings: Vec<(u16, u16)>,
    /// Mount points (host_path, container_path)
    pub mounts: Vec<(String, String)>,
    /// DNS servers
    pub dns: Vec<String>,
    /// Path to the gvproxy Unix socket (if gvproxy networking is active)
    pub gvproxy_socket: Option<String>,
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
    const SEARCH_DIRS: &[&str] = &[
        "/opt/homebrew/lib",
        "/usr/local/lib",
    ];
    for dir in SEARCH_DIRS {
        let path = format!("{}/libkrunfw.5.dylib", dir);
        if std::path::Path::new(&path).exists() {
            if let Ok(cdir) = std::ffi::CString::new(*dir) {
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
    let ent_path = std::env::temp_dir().join(format!(
        "nanosb-entitlements-{}.plist",
        std::process::id()
    ));
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
    /// MCP server configurations to push to the agent-gateway on start
    mcp_servers: HashMap<String, McpServerConfig>,
    /// Path to VM stderr log file (for diagnostics on startup failure)
    vm_log_path: Option<PathBuf>,
    /// Path to SSH private key for this sandbox
    ssh_key_path: Option<PathBuf>,
    /// Host-side port mapped to guest's sshd (port 22)
    ssh_port: Option<u16>,
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
    #[allow(clippy::too_many_arguments)]
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
            preload_libkrunfw();

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

            // Enable libkrun's internal debug logging to stderr (captured via pipe).
            // This uses krun_init_log which bypasses env_logger entirely.
            let _ = ffi::init_log(ffi::KRUN_LOG_TARGET_DEFAULT, ffi::KRUN_LOG_LEVEL_DEBUG);

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
    #[allow(clippy::too_many_arguments)]
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
            preload_libkrunfw();

            unsafe {
                libc::close(stdout_read_fd);
                libc::close(stderr_read_fd);
                libc::dup2(stdout_write_fd, libc::STDOUT_FILENO);
                libc::dup2(stderr_write_fd, libc::STDERR_FILENO);
                libc::close(stdout_write_fd);
                libc::close(stderr_write_fd);
            }

            // Enable libkrun's internal debug logging to stderr (captured via pipe)
            let _ = ffi::init_log(ffi::KRUN_LOG_TARGET_DEFAULT, ffi::KRUN_LOG_LEVEL_DEBUG);

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
            ffi::set_gvproxy_path(ctx, socket_path)?;
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
        if !dns.is_empty() && !env.contains_key("NANOSANDBOX_DNS") {
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
        // IMPORTANT: Do NOT include exec_path as argv[0]. libkrun stores exec_path
        // separately as KRUN_INIT in the kernel cmdline, and init.c overrides
        // exec_argv[0] with KRUN_INIT. Any argv we pass here becomes argv[1..] in
        // the final execvp() call. Including exec_path as argv[0] would cause it
        // to appear as an extra argument (e.g., /bin/sh trying to read itself as
        // a script file).
        //
        // When gvproxy is active AND the command is not agent-gateway/nanosb-init
        // (which do their own network init), wrap in the nanosb-net-init script
        // which configures eth0 then execs the user's command.
        let (exec_path, exec_argv_owned): (&str, Vec<String>);

        let is_gateway = command.contains("agent-gateway")
            || command.contains("nanosb-init")
            || args.iter().any(|a| a.contains("nanosb-init"));
        if gvproxy_socket.is_some() && !is_gateway {
            let init = "/usr/local/bin/nanosb-net-init";
            let mut v = vec![command.to_string()];
            v.extend(args.iter().map(|a| a.to_string()));
            exec_path = init;
            exec_argv_owned = v;
        } else {
            let v: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            exec_path = command;
            exec_argv_owned = v;
        }

        let exec_argv_refs: Vec<&str> = exec_argv_owned.iter().map(|s| s.as_str()).collect();

        // Set exec with explicit environment (don't inherit host env)
        ffi::set_exec(ctx, exec_path, &exec_argv_refs, Some(&env_vars))?;

        // Write .krun_config.json to the rootfs as a fallback mechanism.
        // Note: KRUN_INIT env var (from kernel cmdline) takes priority in
        // init.c, so this config is only used if KRUN_INIT is not set.
        // The "args" field here uses OCI/Docker convention where args[0]
        // is the program name.
        {
            let mut config_args = vec![exec_path.to_string()];
            config_args.extend(exec_argv_owned.iter().cloned());
            let config = serde_json::json!({
                "args": config_args,
                "env": env_vars,
            });
            let config_path = std::path::Path::new(rootfs_path).join(".krun_config.json");
            let _ = std::fs::write(&config_path, config.to_string());
        }

        // Start the VM -- this never returns on success.
        // libkrun's internal logs (enabled via init_log in the caller) will
        // print the reason for any -EINVAL failure to stderr.
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
    /// Generate ephemeral SSH key pair and inject the public key into the rootfs.
    /// Returns the path to the private key on success.
    fn setup_ssh_keys(sandbox_id: &str, rootfs_path: &Path) -> std::result::Result<PathBuf, String> {
        let key_dir = PathBuf::from(format!("/tmp/nanosb-{}-ssh", sandbox_id));
        std::fs::create_dir_all(&key_dir).map_err(|e| format!("mkdir ssh key dir: {}", e))?;
        let key_path = key_dir.join("id_ed25519");

        let status = std::process::Command::new("ssh-keygen")
            .args([
                "-t", "ed25519",
                "-f", key_path.to_str().unwrap(),
                "-N", "",
                "-q",
            ])
            .status()
            .map_err(|e| format!("ssh-keygen spawn: {}", e))?;

        if !status.success() {
            return Err(format!("ssh-keygen exited with {}", status));
        }

        // Read public key and inject into rootfs
        let pub_key = std::fs::read_to_string(key_path.with_extension("pub"))
            .map_err(|e| format!("read public key: {}", e))?;

        let auth_keys_dir = rootfs_path.join("root/.ssh");
        std::fs::create_dir_all(&auth_keys_dir).map_err(|e| format!("mkdir .ssh: {}", e))?;
        std::fs::write(auth_keys_dir.join("authorized_keys"), &pub_key)
            .map_err(|e| format!("write authorized_keys: {}", e))?;

        // Set permissions (ssh is strict about this)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&auth_keys_dir, std::fs::Permissions::from_mode(0o700));
            let _ = std::fs::set_permissions(
                auth_keys_dir.join("authorized_keys"),
                std::fs::Permissions::from_mode(0o600),
            );
            let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
        }

        Ok(key_path)
    }

    /// Get the SSH host port for a sandbox (if available).
    pub fn ssh_port(&self, id: &str) -> Option<u16> {
        self.sandboxes.lock().unwrap()
            .get(id)
            .and_then(|s| s.ssh_port)
    }

    /// Get the SSH private key path for a sandbox (if available).
    pub fn ssh_key_path(&self, id: &str) -> Option<PathBuf> {
        self.sandboxes.lock().unwrap()
            .get(id)
            .and_then(|s| s.ssh_key_path.clone())
    }

    /// Build a ready-to-use SSH command string for connecting to a sandbox.
    pub fn ssh_command(&self, id: &str) -> Option<String> {
        let sandboxes = self.sandboxes.lock().unwrap();
        let state = sandboxes.get(id)?;
        let port = state.ssh_port?;
        let key = state.ssh_key_path.as_ref()?;
        Some(format!(
            "ssh -p {} -i {} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null root@127.0.0.1",
            port,
            key.display(),
        ))
    }

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

        // Write the init script directly into the rootfs. The script is embedded
        // in the binary so it always works regardless of CWD or installation layout.
        // This supersedes any init script from the Docker image.
        {
            let init_dest = rootfs_path.join("usr/local/bin/nanosb-init.sh");
            let _ = std::fs::create_dir_all(rootfs_path.join("usr/local/bin"));
            let script = include_str!("../../docker/nanosb-init.sh");
            if let Err(e) = std::fs::write(&init_dest, script) {
                warn!("Failed to write init script to rootfs: {}", e);
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        &init_dest,
                        std::fs::Permissions::from_mode(0o755),
                    );
                }
                debug!("Embedded init script written to rootfs");
            }
        }

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

        // Detect if the rootfs contains agent-gateway or init script (persistent mode)
        let has_gateway = rootfs_path.join("usr/local/bin/agent-gateway").exists()
            || rootfs_path.join("usr/local/bin/nanosb-init.sh").exists();
        if has_gateway {
            info!(
                "Detected agent-gateway/init script in rootfs for sandbox '{}' -- persistent VM mode enabled",
                id
            );
        }

        // Generate ephemeral SSH key pair and inject public key into rootfs
        let ssh_key_path = if has_gateway {
            match Self::setup_ssh_keys(id, &rootfs_path) {
                Ok(key_path) => {
                    info!("SSH keys generated for sandbox '{}': {}", id, key_path.display());
                    Some(key_path)
                }
                Err(e) => {
                    warn!("Failed to generate SSH keys for sandbox '{}': {} (SSH access will be unavailable)", id, e);
                    None
                }
            }
        } else {
            None
        };

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
            mcp_servers: config.mcp_servers.clone(),
            vm_log_path: None,
            ssh_key_path,
            ssh_port: None,
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

        // Determine PID 1 command: use init script if present, else agent-gateway directly
        let (pid1_command, pid1_args): (String, Vec<String>) = {
            let sandboxes = self.sandboxes.lock().unwrap();
            let state = sandboxes.get(id).unwrap();
            let init_script = state.rootfs_path.join("usr/local/bin/nanosb-init.sh");
            if init_script.exists() {
                // Run init script directly — the kernel's binfmt_script reads
                // the #!/bin/sh shebang and invokes /bin/sh automatically.
                // This avoids issues with how krun_set_exec passes argv through
                // the kernel command line (which can duplicate /bin/sh).
                ("/usr/local/bin/nanosb-init.sh".to_string(), vec![])
            } else {
                ("/usr/local/bin/agent-gateway".to_string(), vec![])
            }
        };

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

        // Store VM log path for diagnostics
        let vm_log_path = PathBuf::from(format!("/tmp/nanosb-{}-vm.log", id));
        {
            let mut sandboxes = self.sandboxes.lock().unwrap();
            if let Some(state) = sandboxes.get_mut(id) {
                state.vm_log_path = Some(vm_log_path.clone());
            }
        }

        // Boot the VM in a blocking task (fork + krun_start_enter)
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
            )
        })
        .await
        .map_err(|e| Error::ExecFailed(format!("Task join error: {}", e)))?
        .map_err(|e| Error::SandboxCreationFailed(format!("VM boot failed: {}", e)))?;

        let id_owned = id.to_string();

        // Expose ports via gvproxy's HTTP API
        let ssh_host_port = {
            let sandboxes = self.sandboxes.lock().unwrap();
            if let Some(state) = sandboxes.get(id) {
                if let Some(ref gvproxy) = state.gvproxy {
                    // Expose gateway port: host_port -> guest:8080
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

                    // Expose SSH port: ssh_host_port -> guest:22
                    if state.ssh_key_path.is_some() {
                        let ssh_port = TcpListener::bind("127.0.0.1:0")
                            .ok()
                            .and_then(|l| l.local_addr().ok())
                            .map(|a| a.port());
                        if let Some(sp) = ssh_port {
                            match gvproxy.expose_port(sp, 22) {
                                Ok(()) => {
                                    info!("Exposed SSH port via gvproxy: host:{} -> guest:22", sp);
                                    Some(sp)
                                }
                                Err(e) => {
                                    warn!("Failed to expose SSH port via gvproxy: {} (SSH access unavailable)", e);
                                    None
                                }
                            }
                        } else {
                            warn!("Failed to find free port for SSH (SSH access unavailable)");
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        };

        // Store the VM PID, gateway port, and SSH port
        {
            let mut sandboxes = self.sandboxes.lock().unwrap();
            if let Some(state) = sandboxes.get_mut(&id_owned) {
                state.gateway_port = Some(host_port);
                state.vm_pid = Some(vm_pid);
                state.ssh_port = ssh_host_port;
            }
        }

        // === Two-stage health check ===
        // Stage 1: Wait for SSH port (fast — sshd starts before agent-gateway)
        // Stage 2: Wait for agent-gateway HTTP health endpoint

        // Helper: check if VM process died during health check
        let check_vm_alive = |vm_pid: i32, vm_log_path: &PathBuf| -> std::result::Result<(), Error> {
            let mut status: libc::c_int = 0;
            let wait_result = unsafe { libc::waitpid(vm_pid, &mut status, libc::WNOHANG) };
            if wait_result > 0 {
                let exit_info = if libc::WIFEXITED(status) {
                    format!("exit code {}", libc::WEXITSTATUS(status))
                } else if libc::WIFSIGNALED(status) {
                    format!("killed by signal {}", libc::WTERMSIG(status))
                } else {
                    format!("status {}", status)
                };
                let vm_log = std::fs::read_to_string(vm_log_path)
                    .unwrap_or_else(|_| "(no log file)".to_string());
                Err(Error::SandboxCreationFailed(format!(
                    "VM process exited during startup ({}).\nVM log:\n{}",
                    exit_info,
                    if vm_log.is_empty() { "(empty)" } else { &vm_log },
                )))
            } else {
                Ok(())
            }
        };

        // Stage 1: Wait for SSH (10s timeout) — proves VM booted + networking works
        if let Some(sp) = ssh_host_port {
            let ssh_addr: std::net::SocketAddr = format!("127.0.0.1:{}", sp).parse().unwrap();
            let ssh_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut ssh_delay_ms = 100u64;

            loop {
                if std::time::Instant::now() > ssh_deadline {
                    let vm_alive = unsafe { libc::kill(vm_pid, 0) == 0 };
                    let vm_log = std::fs::read_to_string(&vm_log_path)
                        .unwrap_or_else(|_| "(no log file)".to_string());
                    let log_tail: String = vm_log.lines().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
                    warn!(
                        "SSH port not reachable after 10s for sandbox '{}' (VM: {}, PID: {}). Continuing to gateway check...\nVM log:\n{}",
                        id_owned,
                        if vm_alive { "alive" } else { "dead" },
                        vm_pid,
                        if log_tail.is_empty() { "(empty)" } else { &log_tail },
                    );
                    break;
                }

                check_vm_alive(vm_pid, &vm_log_path)?;

                if std::net::TcpStream::connect_timeout(
                    &ssh_addr,
                    std::time::Duration::from_millis(500),
                ).is_ok() {
                    info!("SSH port reachable for sandbox '{}' at host port {}", id_owned, sp);
                    break;
                }

                tokio::time::sleep(std::time::Duration::from_millis(ssh_delay_ms)).await;
                ssh_delay_ms = (ssh_delay_ms * 2).min(1000);
            }
        }

        // Stage 2: Wait for agent-gateway HTTP health (30s timeout)
        let health_url = format!("127.0.0.1:{}", host_port);
        let mut delay_ms = 100u64;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);

        loop {
            if std::time::Instant::now() > deadline {
                let vm_alive = unsafe { libc::kill(vm_pid, 0) == 0 };
                let vm_log = std::fs::read_to_string(&vm_log_path)
                    .unwrap_or_else(|_| "(no log file)".to_string());
                let log_tail: String = vm_log.lines().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");

                let diag = format!(
                    "VM health check timed out after 30s.\n\
                     VM process: {} (PID {})\n\
                     gvproxy port forward: host:{} -> guest:8080\n\
                     SSH port: {}\n\
                     VM log (last 20 lines):\n{}",
                    if vm_alive { "alive" } else { "dead" },
                    vm_pid,
                    host_port,
                    ssh_host_port.map(|p| p.to_string()).unwrap_or_else(|| "N/A".to_string()),
                    if log_tail.is_empty() { "(empty)" } else { &log_tail },
                );
                return Err(Error::SandboxCreationFailed(diag));
            }

            check_vm_alive(vm_pid, &vm_log_path)?;

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
            delay_ms = (delay_ms * 2).min(2000);
        }

        // Push MCP server config from SandboxConfig (if any)
        let mcp_servers = {
            let sandboxes = self.sandboxes.lock().unwrap();
            let state = sandboxes.get(id).unwrap();
            state.mcp_servers.clone()
        };

        if !mcp_servers.is_empty() {
            info!(
                "Pushing {} MCP server(s) to gateway for sandbox '{}'",
                mcp_servers.len(),
                id
            );
            if let Err(e) = self.push_mcp_config(id, &mcp_servers) {
                warn!("Failed to push MCP config for sandbox '{}': {}", id, e);
            }
        }

        Ok(())
    }

    /// Boot a persistent VM with agent-gateway as PID 1.
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
        port_mappings: &[(u16, u16)],
        mounts: &[(String, String)],
        dns: &[String],
        gvproxy_socket: Option<&str>,
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
        };

        let config_json = serde_json::to_string(&request)
            .map_err(|e| format!("Failed to serialize boot config: {}", e))?;

        let exe_path = std::env::current_exe()
            .map_err(|e| format!("Failed to get current exe path: {}", e))?;

        // Ensure the binary has the com.apple.security.hypervisor entitlement.
        // The binary may have lost its entitlement if cargo rebuilt it after the
        // initial codesign-and-run.sh signing. Re-signing here guarantees the
        // subprocess will have the entitlement when macOS evaluates it at exec.
        #[cfg(target_os = "macos")]
        ensure_hypervisor_entitlement(&exe_path)?;

        // Spawn the VM in a clean subprocess via posix_spawn (not fork)
        let mut child = std::process::Command::new(&exe_path)
            .arg("internal-boot-vm")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn boot-vm subprocess: {}", e))?;

        // Write config JSON to child's stdin, then close it
        {
            let mut stdin = child.stdin.take()
                .ok_or_else(|| "Failed to open subprocess stdin".to_string())?;
            stdin
                .write_all(config_json.as_bytes())
                .map_err(|e| format!("Failed to write config to subprocess: {}", e))?;
            // stdin drops here, closing the pipe so child gets EOF
        }

        let pid = child.id() as i32;

        // Take stdout/stderr handles for background logging
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let log_path = format!("/tmp/nanosb-{}-vm.log", sandbox_id);

        // Background thread: merge child stdout (VM console = agent output)
        // and stderr (libkrun warnings/errors) into the VM log file.
        // The child runs indefinitely (krun_start_enter never returns on
        // success), so this thread blocks until the VM exits or is killed.
        std::thread::spawn(move || {
            use std::io::Write as _;
            let log_file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&log_path);

            if let Ok(mut log_file) = log_file {
                // Merge both streams line-by-line using two reader threads
                let (tx, rx) = std::sync::mpsc::channel::<String>();

                let tx_out = tx.clone();
                let stdout_thread = stdout.map(|out| {
                    std::thread::spawn(move || {
                        for line in BufReader::new(out).lines() {
                            match line {
                                Ok(l) => { let _ = tx_out.send(l); }
                                Err(_) => break,
                            }
                        }
                    })
                });

                let tx_err = tx;
                let stderr_thread = stderr.map(|err| {
                    std::thread::spawn(move || {
                        for line in BufReader::new(err).lines() {
                            match line {
                                Ok(l) => { let _ = tx_err.send(l); }
                                Err(_) => break,
                            }
                        }
                    })
                });

                // Write lines as they arrive from either stream
                for line in rx {
                    let _ = writeln!(log_file, "{}", line);
                }

                if let Some(t) = stdout_thread { let _ = t.join(); }
                if let Some(t) = stderr_thread { let _ = t.join(); }
            }

            // child drops here. Child::drop does non-blocking waitpid
            // which is harmless — the real cleanup is via SIGKILL in destroy().
            drop(child);
        });

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

    /// Minimal HTTP DELETE using raw TcpStream. Returns (status_code, body).
    fn http_delete(addr: &str, path: &str) -> std::result::Result<(u16, String), String> {
        let mut stream = std::net::TcpStream::connect(addr).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .map_err(|e| e.to_string())?;

        let request = format!(
            "DELETE {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
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
        .map_err(Error::ExecFailed)?;

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
        .map_err(Error::ExecFailed)?;

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
            .map_err(Error::ExecFailed)?;

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
            .map_err(Error::ExecFailed)?;

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

            // Clean up SSH keys
            if let Some(ref key_path) = state.ssh_key_path {
                if let Some(parent) = key_path.parent() {
                    let _ = std::fs::remove_dir_all(parent);
                }
            }

            // Clean up VM log file
            if let Some(ref log_path) = state.vm_log_path {
                let _ = std::fs::remove_file(log_path);
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
            .map_err(Error::ExecFailed)?;

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

    /// Helper: get gateway port or return McpNotSupported error.
    fn require_gateway_port(&self, id: &str) -> Result<u16> {
        let sandboxes = self.sandboxes.lock().unwrap();
        let state = sandboxes
            .get(id)
            .ok_or_else(|| Error::SandboxNotFound(id.to_string()))?;
        state.gateway_port.ok_or_else(|| {
            Error::McpNotSupported(
                "MCP operations require a persistent VM with agent-gateway".to_string(),
            )
        })
    }

    /// Push all MCP server configs to the agent-gateway.
    pub fn push_mcp_config(
        &self,
        id: &str,
        servers: &HashMap<String, McpServerConfig>,
    ) -> Result<()> {
        let gateway_port = self.require_gateway_port(id)?;
        let addr = format!("127.0.0.1:{}", gateway_port);

        for (name, mcp_config) in servers {
            if !mcp_config.enabled {
                continue;
            }

            let body = serde_json::json!({
                "name": name,
                "command": mcp_config.command,
                "args": mcp_config.args,
                "env": mcp_config.env,
                "enabled": true
            });

            match Self::http_post(&addr, "/api/v1/mcp/servers", &body.to_string()) {
                Ok((code, _)) if (200..300).contains(&code) => {
                    info!("Pushed MCP server '{}' to gateway for sandbox '{}'", name, id);
                }
                Ok((code, body)) => {
                    warn!(
                        "Failed to push MCP server '{}' (HTTP {}): {}",
                        name, code, body.chars().take(200).collect::<String>()
                    );
                }
                Err(e) => {
                    warn!("Failed to push MCP server '{}': {}", name, e);
                }
            }
        }

        // Regenerate all agent configs after pushing
        match Self::http_post(&addr, "/api/v1/mcp/regenerate", "{}") {
            Ok((code, _)) if (200..300).contains(&code) => {
                info!("MCP configs regenerated for sandbox '{}'", id);
            }
            Ok((code, body)) => {
                warn!(
                    "Failed to regenerate MCP configs (HTTP {}): {}",
                    code, body.chars().take(200).collect::<String>()
                );
            }
            Err(e) => {
                warn!("Failed to regenerate MCP configs: {}", e);
            }
        }

        Ok(())
    }

    /// Add or update an MCP server in the running sandbox.
    pub fn add_mcp_server(&self, id: &str, name: &str, config: &McpServerConfig) -> Result<()> {
        let gateway_port = self.require_gateway_port(id)?;
        let addr = format!("127.0.0.1:{}", gateway_port);

        let body = serde_json::json!({
            "name": name,
            "command": config.command,
            "args": config.args,
            "env": config.env,
            "enabled": config.enabled
        });

        match Self::http_post(&addr, "/api/v1/mcp/servers", &body.to_string()) {
            Ok((code, _)) if (200..300).contains(&code) => Ok(()),
            Ok((code, body)) => Err(Error::McpServerError(format!(
                "Failed to add MCP server '{}' (HTTP {}): {}",
                name, code, body
            ))),
            Err(e) => Err(Error::McpServerError(format!(
                "Failed to add MCP server '{}': {}",
                name, e
            ))),
        }
    }

    /// Remove an MCP server from the running sandbox.
    pub fn remove_mcp_server(&self, id: &str, name: &str) -> Result<()> {
        let gateway_port = self.require_gateway_port(id)?;
        let addr = format!("127.0.0.1:{}", gateway_port);
        let path = format!("/api/v1/mcp/servers/{}", name);

        match Self::http_delete(&addr, &path) {
            Ok((code, _)) if (200..300).contains(&code) => Ok(()),
            Ok((code, body)) => Err(Error::McpServerError(format!(
                "Failed to remove MCP server '{}' (HTTP {}): {}",
                name, code, body
            ))),
            Err(e) => Err(Error::McpServerError(format!(
                "Failed to remove MCP server '{}': {}",
                name, e
            ))),
        }
    }

    /// List all MCP servers in the running sandbox.
    pub fn list_mcp_servers(&self, id: &str) -> Result<HashMap<String, McpServerConfig>> {
        let gateway_port = self.require_gateway_port(id)?;
        let addr = format!("127.0.0.1:{}", gateway_port);

        match Self::http_get(&addr, "/api/v1/mcp/servers") {
            Ok((code, body)) if (200..300).contains(&code) => {
                let response: serde_json::Value =
                    serde_json::from_str(&body).map_err(|e| {
                        Error::McpServerError(format!("Failed to parse MCP server list: {}", e))
                    })?;

                let servers_val = response.get("servers").unwrap_or(&response);
                let mut result = HashMap::new();

                if let Some(obj) = servers_val.as_object() {
                    for (name, val) in obj {
                        if let Ok(config) = serde_json::from_value::<McpServerConfig>(val.clone()) {
                            result.insert(name.clone(), config);
                        }
                    }
                }

                Ok(result)
            }
            Ok((code, body)) => Err(Error::McpServerError(format!(
                "Failed to list MCP servers (HTTP {}): {}",
                code, body
            ))),
            Err(e) => Err(Error::McpServerError(format!(
                "Failed to list MCP servers: {}",
                e
            ))),
        }
    }

    /// Enable an MCP server in the running sandbox.
    pub fn enable_mcp_server(&self, id: &str, name: &str) -> Result<()> {
        let gateway_port = self.require_gateway_port(id)?;
        let addr = format!("127.0.0.1:{}", gateway_port);
        let path = format!("/api/v1/mcp/servers/{}/enable", name);

        match Self::http_post(&addr, &path, "{}") {
            Ok((code, _)) if (200..300).contains(&code) => Ok(()),
            Ok((code, body)) => Err(Error::McpServerError(format!(
                "Failed to enable MCP server '{}' (HTTP {}): {}",
                name, code, body
            ))),
            Err(e) => Err(Error::McpServerError(format!(
                "Failed to enable MCP server '{}': {}",
                name, e
            ))),
        }
    }

    /// Disable an MCP server in the running sandbox.
    pub fn disable_mcp_server(&self, id: &str, name: &str) -> Result<()> {
        let gateway_port = self.require_gateway_port(id)?;
        let addr = format!("127.0.0.1:{}", gateway_port);
        let path = format!("/api/v1/mcp/servers/{}/disable", name);

        match Self::http_post(&addr, &path, "{}") {
            Ok((code, _)) if (200..300).contains(&code) => Ok(()),
            Ok((code, body)) => Err(Error::McpServerError(format!(
                "Failed to disable MCP server '{}' (HTTP {}): {}",
                name, code, body
            ))),
            Err(e) => Err(Error::McpServerError(format!(
                "Failed to disable MCP server '{}': {}",
                name, e
            ))),
        }
    }
}

// Required for from_raw_fd
use std::os::unix::io::FromRawFd;
