//! Windows libkrun FFI runtime backend
//!
//! Minimal implementation for Windows that uses `std::process::Command` to
//! spawn a subprocess calling krun.dll's C API. Unlike the Unix version which
//! uses fork(), this uses Windows process creation.
//!
//! On Windows, WHPX (Windows Hypervisor Platform) is used instead of KVM/HVF.
//! gvproxy is not available — TSI networking is the only option.

use super::ffi;
use super::gvproxy::GvproxyInstance;
use crate::config::{MountType, NetworkScope, SandboxConfig};
use crate::error::{Error, Result};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use tracing::{debug, error, info, warn};

/// Configuration passed to the `internal-boot-vm` subprocess.
#[derive(Serialize, Deserialize)]
pub struct BootVmRequest {
    pub sandbox_id: String,
    pub rootfs_path: String,
    pub cpus: u32,
    pub memory_mb: u32,
    pub command: String,
    pub command_args: Vec<String>,
    pub network_scope: NetworkScope,
    pub port_mappings: Vec<(u16, u16, String)>,
    pub mounts: Vec<(String, String)>,
    pub dns: Vec<String>,
    /// Custom DNS zone entries (name=IP pairs) for /etc/hosts in guest
    #[serde(default)]
    pub dns_zones: HashMap<String, String>,
    pub gvproxy_socket: Option<String>,
    /// SSH public key to inject via kernel cmdline (for FUSE mode where rootfs perms are broken)
    #[serde(default)]
    pub ssh_pubkey: Option<String>,
    /// Run agent processes as root inside guest.
    #[serde(default)]
    pub run_as_root: bool,
}

/// Add `~/.nanosandbox/libs/` to the Windows DLL search path.
///
/// `libkrunfw.dll` is loaded by libkrun via `libloading::Library::new("libkrunfw.dll")`
/// which uses the standard Windows DLL search order (exe dir, system dirs, PATH).
/// Since the DLL lives in `~/.nanosandbox/libs/`, we must explicitly add that
/// directory so `LoadLibrary` can find it.
fn add_libs_to_dll_search_path() {
    extern "system" {
        fn SetDllDirectoryW(lpPathName: *const u16) -> i32;
    }

    let home = match std::env::var("USERPROFILE") {
        Ok(h) => h,
        Err(_) => return,
    };

    // Prefer libs/ subdirectory, fall back to root level (legacy layout)
    let candidates = [
        PathBuf::from(&home).join(".nanosandbox").join("libs"),
        PathBuf::from(&home).join(".nanosandbox"),
    ];

    for dir in &candidates {
        if dir.join("libkrunfw.dll").exists() {
            let wide: Vec<u16> = dir
                .to_string_lossy()
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            if unsafe { SetDllDirectoryW(wide.as_ptr()) } != 0 {
                eprintln!("internal-boot-vm: DLL search path: {}", dir.display());
                return;
            }
        }
    }
}

/// Derive image blobs cache directory from bundle rootfs path.
///
/// Expected bundle layout is `<cache>/bundles/<sandbox-id>/rootfs`.
/// Returns `<cache>/blobs/sha256` when that structure is present.
fn blobs_dir_from_rootfs(rootfs_path: &Path) -> Option<PathBuf> {
    let bundle_dir = rootfs_path.parent()?;
    let bundles_dir = bundle_dir.parent()?;
    if bundles_dir.file_name()?.to_string_lossy() != "bundles" {
        return None;
    }
    let cache_dir = bundles_dir.parent()?;
    Some(cache_dir.join("blobs").join("sha256"))
}

/// Entry point for the `internal-boot-vm` subprocess on Windows.
pub fn handle_boot_vm_subprocess() -> ! {
    // Add ~/.nanosandbox/libs/ to the DLL search path so libkrunfw.dll can be found.
    // This must happen BEFORE any code triggers the lazy libloading::Library::new() call.
    add_libs_to_dll_search_path();

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

    match ffi::init_log(ffi::KRUN_LOG_TARGET_DEFAULT, ffi::KRUN_LOG_LEVEL_DEBUG) {
        Ok(()) => eprintln!("internal-boot-vm: krun_init_log OK (level=DEBUG, target=stderr)"),
        Err(e) => eprintln!("internal-boot-vm: krun_init_log FAILED: {}", e),
    }

    // Pass SSH pubkey so builder appends it to kernel cmdline as nanosb.ssh_key=...
    if let Some(ref key) = config.ssh_pubkey {
        std::env::set_var("NANOSB_SSH_PUBKEY", key);
    }
    if config.run_as_root {
        std::env::set_var("NANOSB_RUN_AS_ROOT", "1");
    }

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
    // Pass custom DNS zones as "name=ip,name2=ip2" so init.krun can write /etc/hosts
    if !config.dns_zones.is_empty() {
        let zones: String = config
            .dns_zones
            .iter()
            .map(|(name, ip)| format!("{}={}", name, ip))
            .collect::<Vec<_>>()
            .join(",");
        env.insert("NANOSANDBOX_DNS_ZONES".to_string(), zones);
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

/// State tracked per sandbox
#[allow(dead_code)]
struct SandboxState {
    rootfs_path: PathBuf,
    cpus: u32,
    memory_mb: u32,
    dns: Vec<String>,
    dns_zones: HashMap<String, String>,
    network_scope: NetworkScope,
    port_mappings: Vec<(u16, u16, String)>,
    mounts: Vec<(String, String)>,
    gvproxy: Option<GvproxyInstance>,
    vm_child: Option<std::process::Child>,
    guest_ip: Option<String>,
    /// HCN endpoint GUID — used to clean up networking on stop/destroy.
    hcn_endpoint_id: Option<String>,
    /// HCS VM identity string — used for AF_HYPERV (HvSocket) connections.
    hcs_vm_id: Option<String>,
    ssh_pubkey: Option<String>,
    run_as_root: bool,
    /// PID 1 command override (set by sandbox layer via config.command).
    command: Option<String>,
    /// PID 1 command arguments.
    command_args: Vec<String>,
}

/// Direct libkrun FFI runtime for Windows (WHPX)
///
/// Uses `std::process::Command` to spawn VM subprocess (no fork on Windows).
/// TSI networking only (no gvproxy).
pub struct LibkrunRuntime {
    sandboxes: std::sync::Mutex<HashMap<String, SandboxState>>,
}

impl LibkrunRuntime {
    pub async fn new() -> Result<Self> {
        let _ = ffi::set_log_level(3); // WARN level, match Linux/macOS
        Ok(Self {
            sandboxes: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Lock the sandboxes mutex, recovering from poisoning.
    fn lock_sandboxes(&self) -> std::sync::MutexGuard<'_, HashMap<String, SandboxState>> {
        self.sandboxes.lock().unwrap_or_else(|poisoned| {
            warn!("Sandbox mutex was poisoned, recovering");
            poisoned.into_inner()
        })
    }

    pub fn is_available() -> bool {
        ffi::create_ctx().is_ok()
    }

    pub fn handles_image_pull(&self) -> bool {
        false
    }

    pub fn guest_ip(&self, id: &str) -> Option<String> {
        let sandboxes = self.lock_sandboxes();
        sandboxes.get(id).and_then(|s| s.guest_ip.clone())
    }

    /// Get the HCS VM identity for HvSocket connections.
    pub fn hcs_vm_id(&self, id: &str) -> Option<String> {
        let sandboxes = self.lock_sandboxes();
        sandboxes.get(id).and_then(|s| s.hcs_vm_id.clone())
    }

    /// Check if the VM child process is still running.
    pub fn is_vm_running(&self, id: &str) -> bool {
        let mut sandboxes = self.lock_sandboxes();
        if let Some(state) = sandboxes.get_mut(id) {
            if let Some(ref mut child) = state.vm_child {
                // try_wait returns Ok(Some(status)) if exited, Ok(None) if still running
                match child.try_wait() {
                    Ok(None) => true,
                    _ => false,
                }
            } else {
                false
            }
        } else {
            false
        }
    }

    pub fn expose_port(&self, id: &str, port: u16) -> std::result::Result<(), String> {
        // On Windows with HCN, the guest IP is directly reachable from the host
        // without port forwarding. The guest IP + port is accessible as-is.
        // This is different from Linux/macOS where gvproxy maps localhost:port -> guest:port.
        let sandboxes = self.lock_sandboxes();
        if let Some(state) = sandboxes.get(id) {
            if let Some(ref ip) = state.guest_ip {
                info!("Port {} is directly accessible at {}:{} (HCN networking)", port, ip, port);
                Ok(())
            } else {
                Err("Guest IP not available (VM not started?)".into())
            }
        } else {
            Err(format!("Sandbox '{}' not found", id))
        }
    }

    /// Configure and start a libkrun VM (called in subprocess context).
    fn configure_and_start_vm(
        rootfs_path: &str,
        cpus: u32,
        memory_mb: u32,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
        network_scope: &NetworkScope,
        port_mappings: &[(u16, u16, String)],
        mounts: &[(String, String)],
        dns: &[String],
        _gvproxy_socket: Option<&str>,
    ) -> std::result::Result<(), String> {
        let ctx_id = ffi::create_ctx()?;

        // Note: ffi::get_max_vcpus() is Unix-only (krun_get_max_vcpus not exposed on Windows).
        // On Windows/WHPX, cap at 255 (u8 max for set_vm_config) and minimum 1.
        let effective_cpus = cpus.min(255).max(1);
        ffi::set_vm_config(ctx_id, effective_cpus as u8, memory_mb)?;
        ffi::set_root(ctx_id, rootfs_path)?;

        if let Some(wd) = workdir {
            let _ = ffi::set_workdir(ctx_id, wd);
        }

        // Environment
        let env_strings: Vec<String> = env.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
        ffi::set_env(ctx_id, &env_strings)?;

        // Exec — pass env_strings so krun_set_exec doesn't fall back to
        // collecting the host process environment (which leaks Windows PATH etc.).
        ffi::set_exec(ctx_id, command, args, Some(&env_strings))?;

        // Mounts
        for (host, container) in mounts {
            let _ = ffi::add_virtiofs(ctx_id, container, host);
        }

        // TSI networking (always, since no gvproxy on Windows)
        let tsi_features = match network_scope {
            NetworkScope::Any => ffi::KRUN_TSI_HIJACK_INET | ffi::KRUN_TSI_HIJACK_UNIX,
            NetworkScope::Public | NetworkScope::Group => ffi::KRUN_TSI_HIJACK_INET,
            NetworkScope::None => 0,
        };
        if tsi_features > 0 {
            let _ = ffi::add_vsock(ctx_id, tsi_features);
        }

        // Port mappings
        if !port_mappings.is_empty() {
            let mapping_strings: Vec<String> = port_mappings
                .iter()
                .map(|(h, g, _proto)| format!("{}:{}", h, g))
                .collect();
            let _ = ffi::set_port_map(ctx_id, Some(&mapping_strings));
        }

        // Start VM (never returns on success)
        ffi::start_enter(ctx_id)?;
        Ok(())
    }

    /// Launch VM in a subprocess using std::process::Command
    fn launch_vm_subprocess(
        &self,
        sandbox_id: &str,
        rootfs_path: &str,
        cpus: u32,
        memory_mb: u32,
        command: &str,
        args: &[&str],
        network_scope: &NetworkScope,
        port_mappings: &[(u16, u16, String)],
        mounts: &[(String, String)],
        dns: &[String],
        dns_zones: &HashMap<String, String>,
        gvproxy_socket: Option<&str>,
        ssh_pubkey: Option<&str>,
        run_as_root: bool,
    ) -> std::result::Result<std::process::Child, String> {
        let request = BootVmRequest {
            sandbox_id: sandbox_id.to_string(),
            rootfs_path: rootfs_path.to_string(),
            cpus,
            memory_mb,
            command: command.to_string(),
            command_args: args.iter().map(|s| s.to_string()).collect(),
            network_scope: network_scope.clone(),
            port_mappings: port_mappings.to_vec(),
            mounts: mounts.to_vec(),
            dns: dns.to_vec(),
            dns_zones: dns_zones.clone(),
            gvproxy_socket: gvproxy_socket.map(|s| s.to_string()),
            ssh_pubkey: ssh_pubkey.map(|s| s.to_string()),
            run_as_root,
        };

        let json = serde_json::to_string(&request)
            .map_err(|e| format!("Failed to serialize BootVmRequest: {}", e))?;

        let exe = std::env::var("NANOSB_EXE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::current_exe().expect("Failed to get current exe")
            });

        let mut child = std::process::Command::new(&exe)
            .arg("internal-boot-vm")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn VM subprocess: {}", e))?;

        // Write config to stdin
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(json.as_bytes());
        }

        Ok(child)
    }

    pub async fn create(
        &self,
        id: &str,
        config: &SandboxConfig,
        bundle_path: Option<&Path>,
    ) -> Result<()> {
        // Cleanup existing sandbox with the same ID (match Linux/macOS behavior)
        if self.lock_sandboxes().contains_key(id) {
            info!("Sandbox '{}' already exists, destroying before re-create", id);
            let _ = self.destroy(id).await;
        }

        let bundle_dir = bundle_path.ok_or_else(|| {
            error!("Sandbox '{}': bundle_path is required but was not provided", id);
            Error::SandboxCreationFailed("bundle_path is required".to_string())
        })?;
        let rootfs_path = bundle_dir.join("rootfs");

        // Validate rootfs exists
        if !rootfs_path.exists() {
            error!("Sandbox '{}': rootfs does not exist: {}", id, rootfs_path.display());
            return Err(Error::SandboxCreationFailed(format!(
                "rootfs does not exist: {}",
                rootfs_path.display()
            )));
        }

        // Remove stale config.json to prevent libkrun from reading old commands
        let config_json = bundle_dir.join("config.json");
        if config_json.exists() {
            let _ = std::fs::remove_file(&config_json);
        }

        // Filter mounts by type (VirtioFs + Bind only) and canonicalize paths
        let mounts: Vec<(String, String)> = config
            .mounts
            .iter()
            .filter(|m| {
                matches!(m.mount_type, MountType::VirtioFs | MountType::Bind)
            })
            .map(|m| {
                let host = m
                    .host_path
                    .canonicalize()
                    .unwrap_or_else(|_| m.host_path.clone())
                    .to_string_lossy()
                    .to_string();
                (host, m.container_path.clone())
            })
            .collect();

        // Write virtiofs mount config for guest init script
        if !mounts.is_empty() {
            let mount_config_path = rootfs_path.join("etc/nanosb-mounts");
            if let Some(parent) = mount_config_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let mount_config: String = mounts
                .iter()
                .enumerate()
                .map(|(i, (_, container))| format!("mount{}:{}", i, container))
                .collect::<Vec<_>>()
                .join("\n");
            let _ = std::fs::write(&mount_config_path, &mount_config);

            // Write FUSE workspace mount config for fuse_mount.
            // Format: <port>:<container_path>
            // Example: 50010:/workspace
            let fuse_mount_config_path = rootfs_path.join("etc/nanosb-fuse-mounts");
            let fuse_mount_config: String = mounts
                .iter()
                .enumerate()
                .take(crate::hvsocket::HVSOCK_FUSE_WORKSPACE_MAX_SHARES as usize)
                .map(|(i, (_, container))| {
                    let port = crate::hvsocket::HVSOCK_FUSE_WORKSPACE_BASE_PORT + i as u32;
                    format!("{}:{}", port, container)
                })
                .collect::<Vec<_>>()
                .join("\n");
            let _ = std::fs::write(&fuse_mount_config_path, &fuse_mount_config);
        }

        // DNS defaults: add public DNS if network enabled but none specified
        let mut dns = config.network.dns.clone();
        if dns.is_empty() && !matches!(config.network.scope, NetworkScope::None) {
            dns.push("8.8.8.8".to_string());
            dns.push("1.1.1.1".to_string());
        }

        let state = SandboxState {
            rootfs_path,
            cpus: config.cpus,
            memory_mb: config.memory_mb,
            dns,
            dns_zones: config.network.dns_zones.clone(),
            network_scope: config.network.scope.clone(),
            port_mappings: config
                .network
                .port_mappings
                .iter()
                .map(|pm| (pm.host_port, pm.container_port, pm.protocol.clone()))
                .collect(),
            mounts,
            gvproxy: None,
            vm_child: None,
            guest_ip: None,
            hcn_endpoint_id: None,
            hcs_vm_id: None,
            ssh_pubkey: config.ssh_pubkey.clone(),
            run_as_root: config.run_as_root,
            command: config.command.clone(),
            command_args: config.command_args.clone(),
        };

        self.lock_sandboxes().insert(id.to_string(), state);
        info!("Created sandbox '{}' (Windows/HCS, fuse rootfs)", id);
        Ok(())
    }

    pub async fn start(&self, id: &str) -> Result<()> {
        // Read sandbox state for VM boot
        let (rootfs_path, cpus, memory_mb, network_scope, port_mappings, mounts, dns, dns_zones, ssh_pubkey, run_as_root) = {
            let sandboxes = self.lock_sandboxes();
            let state = sandboxes.get(id).ok_or_else(|| {
                error!("Sandbox '{}' not found in start()", id);
                Error::SandboxNotFound(id.to_string())
            })?;
            (
                state.rootfs_path.to_string_lossy().to_string(),
                state.cpus,
                state.memory_mb,
                state.network_scope.clone(),
                state.port_mappings.clone(),
                state.mounts.clone(),
                state.dns.clone(),
                state.dns_zones.clone(),
                state.ssh_pubkey.clone(),
                state.run_as_root,
            )
        };

        // PID 1 command: use config.command if set by the sandbox layer,
        // otherwise fall back to a sleep hold to keep the VM alive.
        let (command_owned, args_owned): (String, Vec<String>) = {
            let sandboxes = self.lock_sandboxes();
            let state = sandboxes.get(id).ok_or_else(|| Error::SandboxNotFound(id.to_string()))?;
            if let Some(ref cmd) = state.command {
                info!("VM: PID 1 = {}", cmd);
                (cmd.clone(), state.command_args.clone())
            } else {
                info!("VM: no command set, using sleep hold");
                ("/bin/sh".to_string(), vec!["-c".to_string(), "while true; do sleep 3600; done".to_string()])
            }
        };
        let command = command_owned.as_str();
        let args: Vec<&str> = args_owned.iter().map(|s| s.as_str()).collect();

        let mut child = self.launch_vm_subprocess(
            id,
            &rootfs_path,
            cpus,
            memory_mb,
            command,
            &args,
            &network_scope,
            &port_mappings,
            &mounts,
            &dns,
            &dns_zones,
            None,
            ssh_pubkey.as_deref(),
            run_as_root,
        ).map_err(|e| {
            error!("Failed to launch VM subprocess for sandbox '{}': {}", id, e);
            Error::SandboxCreationFailed(e)
        })?;

        // Spawn a background thread to drain VM subprocess stderr into the
        // tracing log. File logging is the CLI's responsibility — the runtime
        // emits via tracing only and the application's subscriber decides
        // where it lands.
        let sandbox_id_for_stderr = id.to_string();
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().flatten() {
                    info!("[vm-{}] {}", &sandbox_id_for_stderr[..8.min(sandbox_id_for_stderr.len())], line);
                }
            });
        }

        // Read the guest IP, HCN endpoint ID, and HCS VM ID from subprocess stdout.
        // The builder prints these as key=value lines before entering the VM.
        let mut guest_ip = None;
        let mut hcn_endpoint_id = None;
        let mut hcs_vm_id = None;
        if let Some(ref mut stdout) = child.stdout {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        info!("[vm-stdout] {}", l);
                        if let Some(ip) = l.strip_prefix("NANOSB_GUEST_IP=") {
                            guest_ip = Some(ip.trim().to_string());
                        } else if let Some(eid) = l.strip_prefix("NANOSB_ENDPOINT_ID=") {
                            hcn_endpoint_id = Some(eid.trim().to_string());
                        } else if let Some(vid) = l.strip_prefix("NANOSB_HCS_VM_ID=") {
                            hcs_vm_id = Some(vid.trim().to_string());
                        }
                        // Stop once we have both VM ID and guest IP.
                        // Guest IP is needed for TCP networking.
                        if hcs_vm_id.is_some() && guest_ip.is_some() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }

        // Continue draining stdout in a background thread (tracing only —
        // file logging is the CLI's concern).
        if let Some(stdout) = child.stdout.take() {
            let sandbox_id_for_stdout = id.to_string();
            std::thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().flatten() {
                    info!("[vm-{}-stdout] {}", &sandbox_id_for_stdout[..8.min(sandbox_id_for_stdout.len())], line);
                }
            });
        }

        if let Some(ref ip) = guest_ip {
            info!("Guest IP for sandbox '{}': {}", id, ip);
        }
        if let Some(ref eid) = hcn_endpoint_id {
            info!("HCN endpoint for sandbox '{}': {}", id, eid);
        }
        if let Some(ref vid) = hcs_vm_id {
            info!("HCS VM ID for sandbox '{}': {}", id, vid);
        }

        // Start host-side FUSE rootfs server over HvSocket (vsock port 50000).
        // The guest's /bin/fuse_mount connects to this server during early boot.
        if let Some(ref vid) = hcs_vm_id {
            crate::hvsocket::start_fuse_rootfs_server(vid, Path::new(&rootfs_path)).map_err(|e| {
                error!(
                    "Failed to start FUSE rootfs server for sandbox '{}' (vm_id={}): {}",
                    id, vid, e
                );
                Error::SandboxCreationFailed(format!("failed to start FUSE rootfs server: {e}"))
            })?;
            info!("Started FUSE rootfs server for sandbox '{}'", id);

            // If in-guest extraction mode is active (.nanosb-layers present),
            // also serve the blobs cache over HvSocket (vsock port 50002).
            let rootfs = Path::new(&rootfs_path);
            if rootfs.join(".nanosb-layers").exists() {
                let blobs_dir = blobs_dir_from_rootfs(rootfs).ok_or_else(|| {
                    let msg = format!(
                        "failed to derive blobs dir from rootfs path {}",
                        rootfs.display()
                    );
                    error!("Sandbox '{}': {}", id, msg);
                    Error::SandboxCreationFailed(msg)
                })?;

                if !blobs_dir.exists() {
                    let msg = format!("blobs dir does not exist: {}", blobs_dir.display());
                    error!("Sandbox '{}': {}", id, msg);
                    return Err(Error::SandboxCreationFailed(msg));
                }

                crate::hvsocket::start_fuse_blobs_server(vid, &blobs_dir).map_err(|e| {
                    error!(
                        "Failed to start FUSE blobs server for sandbox '{}' (vm_id={}, blobs={}): {}",
                        id,
                        vid,
                        blobs_dir.display(),
                        e
                    );
                    Error::SandboxCreationFailed(format!("failed to start FUSE blobs server: {e}"))
                })?;
                info!(
                    "Started FUSE blobs server for sandbox '{}' (blobs={})",
                    id,
                    blobs_dir.display()
                );
            }

            // Start per-workspace FUSE servers (ports 50010+N).
            if mounts.len() > crate::hvsocket::HVSOCK_FUSE_WORKSPACE_MAX_SHARES as usize {
                warn!(
                    "Sandbox '{}': {} mounts configured, truncating to {} FUSE workspace shares",
                    id,
                    mounts.len(),
                    crate::hvsocket::HVSOCK_FUSE_WORKSPACE_MAX_SHARES
                );
            }
            for (i, (host_path, container_path)) in mounts
                .iter()
                .enumerate()
                .take(crate::hvsocket::HVSOCK_FUSE_WORKSPACE_MAX_SHARES as usize)
            {
                let port = crate::hvsocket::HVSOCK_FUSE_WORKSPACE_BASE_PORT + i as u32;
                let host = Path::new(host_path);
                crate::hvsocket::start_fuse_workspace_server(vid, host, port).map_err(|e| {
                    error!(
                        "Failed to start FUSE workspace server for sandbox '{}' (vm_id={}, port={}, host={}, container={}): {}",
                        id,
                        vid,
                        port,
                        host.display(),
                        container_path,
                        e
                    );
                    Error::SandboxCreationFailed(format!(
                        "failed to start FUSE workspace server on port {}: {}",
                        port, e
                    ))
                })?;
                info!(
                    "Started FUSE workspace server for sandbox '{}' (port={}, host={}, container={})",
                    id,
                    port,
                    host.display(),
                    container_path
                );
            }
        } else {
            warn!(
                "Sandbox '{}': HCS VM ID missing; cannot start FUSE rootfs server on port 50000",
                id
            );
        }

        // Register HvSocket service GUID (one-time, requires admin).
        crate::hvsocket::ensure_hvsock_service_registered();

        // Store the child process, guest IP, HCN endpoint ID, and HCS VM ID.
        {
            let mut sandboxes = self.lock_sandboxes();
            if let Some(state) = sandboxes.get_mut(id) {
                state.vm_child = Some(child);
                state.guest_ip = guest_ip.clone();
                state.hcn_endpoint_id = hcn_endpoint_id;
                state.hcs_vm_id = hcs_vm_id;
            }
        }

        info!("Sandbox '{}' ready (Windows/HCS)", id);
        Ok(())
    }

    pub async fn stop(&self, id: &str) -> Result<()> {
        // Kill VM child process and clean up HCN endpoint.
        let endpoint_id = {
            let mut sandboxes = self.lock_sandboxes();
            if let Some(state) = sandboxes.get_mut(id) {
                if let Some(ref mut child) = state.vm_child {
                    let pid = child.id();
                    let _ = child.kill();
                    // Wait with timeout — don't hang if the process won't die
                    let wait_start = std::time::Instant::now();
                    loop {
                        match child.try_wait() {
                            Ok(Some(_)) => break,
                            Ok(None) if wait_start.elapsed() < std::time::Duration::from_secs(5) => {
                                std::thread::sleep(std::time::Duration::from_millis(100));
                            }
                            _ => {
                                warn!("Sandbox '{}': VM process {} did not exit within 5s after kill", id, pid);
                                break;
                            }
                        }
                    }
                }
                state.vm_child = None;
                state.hcn_endpoint_id.take()
            } else {
                None
            }
        };

        // Delete the HCN endpoint from the parent process.
        // The subprocess's Drop impl may not run when the process is killed,
        // leaving stale endpoints that block port reuse on subsequent runs.
        // Use spawn + wait_with_output with timeout to avoid hanging.
        if let Some(ref eid) = endpoint_id {
            match std::process::Command::new("hnsdiag.exe")
                .args(["delete", "endpoints", eid])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    let start = std::time::Instant::now();
                    loop {
                        match child.try_wait() {
                            Ok(Some(status)) if status.success() => {
                                info!("Deleted HCN endpoint {} for sandbox '{}'", eid, id);
                                break;
                            }
                            Ok(Some(status)) => {
                                warn!("hnsdiag delete endpoint {} exited with {} for sandbox '{}'", eid, status, id);
                                break;
                            }
                            Ok(None) if start.elapsed() < std::time::Duration::from_secs(5) => {
                                std::thread::sleep(std::time::Duration::from_millis(100));
                            }
                            _ => {
                                warn!("hnsdiag timed out deleting endpoint {} for sandbox '{}', killing", eid, id);
                                let _ = child.kill();
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to run hnsdiag to delete endpoint {} for sandbox '{}': {}",
                        eid, id, e
                    );
                }
            }
        }

        info!("Stopped sandbox '{}'", id);
        Ok(())
    }

    pub async fn destroy(&self, id: &str) -> Result<()> {
        self.stop(id).await?;

        let removed = self.lock_sandboxes().remove(id);
        if let Some(state) = removed {
            // Keep VM log file for post-mortem debugging.
            // Old logs are cleaned up by the 7-day retention in logging::cleanup_old_logs().

            info!(
                "Destroyed sandbox '{}' (rootfs was: {})",
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

}
