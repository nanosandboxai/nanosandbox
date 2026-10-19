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
use crate::config::{
    ConsoleSpec, ExtraMount, Mount, MountType, NetworkScope, RootfsPatchConfig, RootfsSource,
    RuntimeMode, SandboxConfig, DockerConfig,
};
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
    /// Guest path to the exec agent to run alongside the PID-1 workload.
    #[serde(default)]
    pub exec_agent: Option<String>,
    /// Rootfs source: "oci", "local_dir", or "disk_image".
    #[serde(default = "default_rootfs_source")]
    pub rootfs_source: String,
    /// Disk-image format for a `disk_image` rootfs ("raw"/"qcow2"/"vmdk").
    #[serde(default)]
    pub rootfs_format: Option<String>,
    /// Whether the root disk is exposed read-only.
    #[serde(default)]
    pub rootfs_read_only: bool,
    /// Disk-image mounts (block_id, host_path, format, read_only).
    #[serde(default)]
    pub disk_mounts: Vec<(String, String, String, bool)>,
    /// Pre-boot rootfs patches applied host-side before boot.
    #[serde(default)]
    pub patches: Vec<RootfsPatchConfig>,
    /// Named scripts materialized at `/.nanosb/scripts/<name>` before boot.
    #[serde(default)]
    pub scripts: HashMap<String, String>,
    /// Full mount specs for guest env serialization (NANOSB_MOUNTS).
    #[serde(default)]
    pub mount_specs: Vec<Mount>,
    /// Guest hostname override.
    #[serde(default)]
    pub hostname: Option<String>,
    /// Guest security profile (`default`/`restricted`).
    #[serde(default = "default_security_profile")]
    pub security_profile: String,
    /// Stat virtualization mode (`strict`/`relaxed`/`off`).
    #[serde(default)]
    pub stat_virtualization: Option<String>,
    /// VM-level rlimits passed to the guest init (`RESOURCE=soft:hard`).
    #[serde(default)]
    pub rlimits: Vec<String>,
    /// Docker-in-VM configuration (Epic 7 D5). Omitted from the wire when
    /// docker is not configured so a sandbox without docker has a
    /// byte-identical boot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker: Option<DockerConfig>,
}

fn default_runtime_mode() -> String {
    "legacy".to_string()
}

fn default_rootfs_source() -> String {
    "oci".to_string()
}

fn default_security_profile() -> String {
    "default".to_string()
}

/// Build the optional `NANOSB_DOCKER` guest env pair.
fn docker_guest_env(docker: Option<&DockerConfig>) -> Option<(String, String)> {
    docker.and_then(|d| match serde_json::to_string(d) {
        Ok(json) => Some(("NANOSB_DOCKER".to_string(), json)),
        Err(e) => {
            warn!("Failed to serialize docker config for guest: {}", e);
            None
        }
    })
}

/// Build the optional `DOCKER_HOST` guest env pair when Docker is enabled.
fn docker_host_env(docker: Option<&DockerConfig>) -> Option<(String, String)> {
    docker.filter(|d| d.enabled).map(|d| {
        (
            "DOCKER_HOST".to_string(),
            format!("unix://{}", d.socket),
        )
    })
}

/// Serialize `config.mounts` into the `NANOSB_MOUNTS` guest env value.
///
/// Produces a JSON array of `{target, kind, tag?, source?, fstype?, options[],
/// uid?, gid?, readonly}`. `kind` is one of `virtiofs`, `tmpfs`, `file`, or
/// `disk`. Tags mirror the host-side registration in `create()`: virtiofs
/// mounts get `mountN`, disk-image mounts get `diskN`.
fn mounts_env_json(mounts: &[Mount]) -> serde_json::Value {
    let mut virtiofs_idx = 0usize;
    let mut disk_idx = 0usize;
    let arr: Vec<serde_json::Value> = mounts
        .iter()
        .map(|m| {
            let (kind, tag) = match m.mount_type {
                MountType::Tmpfs => ("tmpfs", None),
                MountType::VirtioFs => {
                    let t = format!("mount{}", virtiofs_idx);
                    virtiofs_idx += 1;
                    ("virtiofs", Some(t))
                }
                MountType::Bind => {
                    if m.disk_image.is_some() {
                        let t = format!("disk{}", disk_idx);
                        disk_idx += 1;
                        ("disk", Some(t))
                    } else if m.file {
                        let t = format!("mount{}", virtiofs_idx);
                        virtiofs_idx += 1;
                        ("file", Some(t))
                    } else {
                        let t = format!("mount{}", virtiofs_idx);
                        virtiofs_idx += 1;
                        ("virtiofs", Some(t))
                    }
                }
            };
            let mut obj = serde_json::Map::new();
            obj.insert(
                "target".into(),
                serde_json::Value::String(m.container_path.clone()),
            );
            obj.insert("kind".into(), serde_json::Value::String(kind.to_string()));
            if let Some(t) = tag {
                obj.insert("tag".into(), serde_json::Value::String(t));
            }
            if m.mount_type != MountType::Tmpfs {
                obj.insert(
                    "source".into(),
                    serde_json::Value::String(m.host_path.to_string_lossy().to_string()),
                );
            }
            if let Some(fs) = &m.fstype {
                obj.insert("fstype".into(), serde_json::Value::String(fs.clone()));
            }
            obj.insert(
                "options".into(),
                serde_json::Value::Array(
                    m.options
                        .iter()
                        .map(|o| serde_json::Value::String(o.clone()))
                        .collect(),
                ),
            );
            if let Some(uid) = m.uid {
                obj.insert("uid".into(), serde_json::Value::Number(uid.into()));
            }
            if let Some(gid) = m.gid {
                obj.insert("gid".into(), serde_json::Value::Number(gid.into()));
            }
            obj.insert("readonly".into(), serde_json::Value::Bool(m.readonly));
            serde_json::Value::Object(obj)
        })
        .collect();
    serde_json::Value::Array(arr)
}

/// Join a guest-relative path onto the rootfs, rejecting any `..` component
/// that would escape the rootfs directory.
fn rootfs_join(rootfs: &Path, guest_path: &str) -> std::result::Result<PathBuf, String> {
    let p = Path::new(guest_path);
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("path escapes rootfs: '{}'", guest_path));
    }
    let rel = p.strip_prefix("/").unwrap_or(p);
    Ok(rootfs.join(rel))
}

/// Apply pre-boot rootfs patches host-side.
///
/// Supported ops: `Mkdir`, `Text`, `CopyFile`, `CopyDir`, `Remove`. All
/// guest-side paths are confined to the rootfs (path escape is rejected).
/// `mode` (when set) is applied via chmod.
fn apply_rootfs_patches(
    rootfs: &Path,
    patches: &[RootfsPatchConfig],
) -> std::result::Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    for patch in patches {
        match patch {
            RootfsPatchConfig::Mkdir { path, mode } => {
                let target = rootfs_join(rootfs, path)?;
                std::fs::create_dir_all(&target)
                    .map_err(|e| format!("mkdir {}: {}", target.display(), e))?;
                if let Some(m) = mode {
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(*m))
                        .map_err(|e| format!("chmod {}: {}", target.display(), e))?;
                }
            }
            RootfsPatchConfig::Text {
                path,
                content,
                mode,
            } => {
                let target = rootfs_join(rootfs, path)?;
                std::fs::create_dir_all(target.parent().unwrap_or(rootfs))
                    .map_err(|e| format!("mkdir {}: {}", target.display(), e))?;
                std::fs::write(&target, content)
                    .map_err(|e| format!("write {}: {}", target.display(), e))?;
                if let Some(m) = mode {
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(*m))
                        .map_err(|e| format!("chmod {}: {}", target.display(), e))?;
                }
            }
            RootfsPatchConfig::CopyFile { from, to, mode } => {
                let target = rootfs_join(rootfs, to)?;
                std::fs::create_dir_all(target.parent().unwrap_or(rootfs))
                    .map_err(|e| format!("mkdir {}: {}", target.display(), e))?;
                std::fs::copy(Path::new(from), &target)
                    .map_err(|e| format!("copy {} -> {}: {}", from, target.display(), e))?;
                if let Some(m) = mode {
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(*m))
                        .map_err(|e| format!("chmod {}: {}", target.display(), e))?;
                }
            }
            RootfsPatchConfig::CopyDir { from, to } => {
                let target = rootfs_join(rootfs, to)?;
                copy_dir_recursive(Path::new(from), &target)
                    .map_err(|e| format!("copy dir {} -> {}: {}", from, target.display(), e))?;
            }
            RootfsPatchConfig::Remove { path, recursive } => {
                let target = rootfs_join(rootfs, path)?;
                if *recursive {
                    std::fs::remove_dir_all(&target)
                        .map_err(|e| format!("remove {}: {}", target.display(), e))?;
                } else {
                    std::fs::remove_file(&target)
                        .map_err(|e| format!("remove {}: {}", target.display(), e))?;
                }
            }
        }
    }
    Ok(())
}

/// Materialize named scripts at `<rootfs>/.nanosb/scripts/<name>` (mode 0755).
fn materialize_scripts(
    rootfs: &Path,
    scripts: &HashMap<String, String>,
) -> std::result::Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let scripts_dir = rootfs.join(".nanosb").join("scripts");
    std::fs::create_dir_all(&scripts_dir)
        .map_err(|e| format!("mkdir {}: {}", scripts_dir.display(), e))?;
    for (name, content) in scripts {
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(format!("invalid script name '{}'", name));
        }
        let path = scripts_dir.join(name);
        std::fs::write(&path, content).map_err(|e| format!("write {}: {}", path.display(), e))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {}", path.display(), e))?;
    }
    Ok(())
}

/// Look up a group name in the guest's `/etc/group`, returning its numeric gid.
fn lookup_group_gid(
    rootfs_path: &std::path::Path,
    name: &str,
) -> std::result::Result<String, String> {
    let path = rootfs_path.join("etc/group");
    let data =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {}", path.display(), e))?;
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
    let default_gid = entry_fields
        .get(3)
        .filter(|g| !g.is_empty())
        .map(|g| g.to_string());
    let passwd_home = entry_fields
        .get(5)
        .filter(|h| !h.is_empty())
        .map(|h| h.to_string());

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

    // Confine the in-process virtiofs server (defense-in-depth): the guest is
    // untrusted and libkrun's virtiofs has no path confinement of its own.
    // Applied before any mount is served. Best-effort — see `sandbox.rs`.
    #[cfg(target_os = "macos")]
    {
        let firmware_dir = std::env::var("HOME")
            .ok()
            .map(|h| format!("{}/.nanosandbox/libs", h));
        let mut writable: Vec<String> = Vec::new();
        if let Some(sock) = config.gvproxy_socket.as_ref() {
            writable.push(sock.clone());
        }
        if let Some(sock) = config.vsock_socket.as_ref() {
            writable.push(sock.clone());
        }
        let mounts = config
            .mounts
            .iter()
            .map(|(host, _container)| (host.clone(), false))
            .chain(
                config
                    .disk_mounts
                    .iter()
                    .map(|(_id, path, _format, readonly)| (path.clone(), *readonly)),
            )
            .chain(
                config
                    .extra_mounts
                    .iter()
                    .map(|m| (m.host_path.clone(), m.readonly)),
            );
        let paths = super::sandbox::VmSandboxPaths::from_parts(
            &config.rootfs_path,
            mounts,
            firmware_dir,
            writable.into_iter(),
        );
        paths.apply();
    }

    // macOS dlopen workaround: chdir to the directory containing libkrunfw
    preload_libkrunfw();

    // Enable libkrun's internal logging to stderr (captured by parent).
    // WARN level avoids the very verbose vCPU MMIO/interrupt traces from DEBUG.
    let _ = ffi::init_log(ffi::KRUN_LOG_TARGET_DEFAULT, ffi::KRUN_LOG_LEVEL_WARN);

    // Pre-boot rootfs customization (directory rootfs only — a disk-image
    // rootfs has no host-visible directory to patch).
    if std::path::Path::new(&config.rootfs_path).is_dir() {
        let rootfs = std::path::Path::new(&config.rootfs_path);
        if let Err(e) = apply_rootfs_patches(rootfs, &config.patches) {
            eprintln!("internal-boot-vm: failed to apply rootfs patches: {}", e);
            std::process::exit(1);
        }
        if let Err(e) = materialize_scripts(rootfs, &config.scripts) {
            eprintln!("internal-boot-vm: failed to materialize scripts: {}", e);
            std::process::exit(1);
        }
    }

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
        // The interactive/next path drives a real 256-color PTY, so a color-capable
        // TERM is required for the agent's native colors; the legacy console is not
        // a full terminal, so it keeps `dumb`.
        let term = if config.runtime_mode == "next" {
            "xterm-256color"
        } else {
            "dumb"
        };
        env.insert("TERM".to_string(), term.to_string());
    }
    if config.runtime_mode == "next" && !env.contains_key("COLORTERM") {
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
    }
    if config.runtime_mode == "next" && !env.contains_key("COLORFGBG") {
        // Signal a black background so agent UIs that probe it pick dark colours.
        env.insert("COLORFGBG".to_string(), "15;0".to_string());
    }
    if !config.dns.is_empty() && !env.contains_key("NANOSANDBOX_DNS") {
        env.insert("NANOSANDBOX_DNS".to_string(), config.dns.join(","));
    }

    // Inject nanosandbox guest environment (both legacy and next paths).
    if let Some(hostname) = &config.hostname {
        env.insert("NANOSB_HOSTNAME".to_string(), hostname.clone());
    }
    env.insert(
        "NANOSB_SECURITY_PROFILE".to_string(),
        config.security_profile.clone(),
    );
    if let Some(stat_virt) = &config.stat_virtualization {
        env.insert("NANOSB_STAT_VIRT".to_string(), stat_virt.clone());
    }
    env.insert(
        "NANOSB_RLIMITS".to_string(),
        serde_json::to_string(&config.rlimits).unwrap_or_default(),
    );
    env.insert(
        "NANOSB_MOUNTS".to_string(),
        serde_json::to_string(&mounts_env_json(&config.mount_specs)).unwrap_or_default(),
    );
    if let Some((key, value)) = docker_guest_env(config.docker.as_ref()) {
        env.insert(key, value);
    }
    if let Some((key, value)) = docker_host_env(config.docker.as_ref()) {
        env.insert(key, value);
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
            config.exec_agent.as_deref(),
            &config.rootfs_source,
            config.rootfs_format.as_deref(),
            config.rootfs_read_only,
            &config.disk_mounts,
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
            &config.rootfs_source,
            config.rootfs_format.as_deref(),
            config.rootfs_read_only,
            &config.disk_mounts,
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
    // NANOSB_HOME (default ~/.nanosandbox) is authoritative, then HOME, then system.
    let mut search_dirs: Vec<String> = Vec::new();
    let nanosb_home = std::env::var("NANOSB_HOME").ok().or_else(|| {
        std::env::var("HOME")
            .ok()
            .map(|h| format!("{h}/.nanosandbox"))
    });
    if let Some(base) = &nanosb_home {
        search_dirs.push(format!("{base}/libs"));
    }
    if let Ok(home) = std::env::var("HOME") {
        search_dirs.push(format!("{}/.nanosandbox/libs", home));
    }
    search_dirs.extend([
        "/opt/homebrew/lib".to_string(),
        "/usr/local/lib".to_string(),
    ]);

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
/// Clone `src` to `dst`, preferring an APFS copy-on-write clone (instant, no
/// extra disk) and falling back to a recursive copy.
fn clone_rootfs(src: &std::path::Path, dst: &std::path::Path) -> std::result::Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let c_src = std::ffi::CString::new(src.as_os_str().as_bytes())
            .map_err(|_| "src path contains NUL".to_string())?;
        let c_dst = std::ffi::CString::new(dst.as_os_str().as_bytes())
            .map_err(|_| "dst path contains NUL".to_string())?;
        if unsafe { libc::clonefile(c_src.as_ptr(), c_dst.as_ptr(), 0) } == 0 {
            return Ok(());
        }
    }
    copy_dir_recursive(src, dst)
}

fn copy_dir_recursive(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> std::result::Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        let to = dst.join(entry.file_name());
        if ty.is_symlink() {
            let target = std::fs::read_link(entry.path()).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&to);
            std::os::unix::fs::symlink(&target, &to).map_err(|e| e.to_string())?;
        } else if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Boot-time configuration carried from the sandbox state into the VM
/// subprocess (serialized into [`BootVmRequest`]).
#[derive(Clone)]
struct BootConfig {
    /// Rootfs source: "oci", "local_dir", or "disk_image".
    rootfs_source: String,
    /// Disk-image format for a `disk_image` rootfs ("raw"/"qcow2"/"vmdk").
    rootfs_format: Option<String>,
    /// Whether the root disk is exposed read-only.
    rootfs_read_only: bool,
    /// Disk-image mounts (block_id, host_path, format, read_only).
    disk_mounts: Vec<(String, String, String, bool)>,
    /// Pre-boot rootfs patches applied host-side before boot.
    patches: Vec<RootfsPatchConfig>,
    /// Named scripts materialized at `/.nanosb/scripts/<name>` before boot.
    scripts: HashMap<String, String>,
    /// Full mount specs for guest env serialization (NANOSB_MOUNTS).
    mount_specs: Vec<Mount>,
    /// Guest hostname override.
    hostname: Option<String>,
    /// Guest security profile (`default`/`restricted`).
    security_profile: String,
    /// Stat virtualization mode (`strict`/`relaxed`/`off`).
    stat_virtualization: Option<String>,
    /// VM-level rlimits passed to the guest init (`RESOURCE=soft:hard`).
    rlimits: Vec<String>,
    /// Docker-in-VM configuration (Epic 7 D5); None means disabled.
    docker: Option<DockerConfig>,
}

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
    /// Guest path to the exec agent run alongside the PID-1 workload.
    exec_agent: Option<String>,
    /// Epic 5 boot-time configuration (rootfs source, disk mounts, patches,
    /// scripts, guest env metadata).
    boot: BootConfig,
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
        rootfs_source: &str,
        rootfs_format: Option<&str>,
        rootfs_read_only: bool,
        disk_mounts: &[(String, String, String, bool)],
    ) -> std::result::Result<(), String> {
        // Create VM context
        let ctx = ffi::create_ctx()?;

        // Configure VM resources — clamp vCPUs to hypervisor max (kernel CONFIG_NR_CPUS).
        // libkrunfw defaults to CONFIG_NR_CPUS=8; query at runtime for the actual limit.
        let max_vcpus = ffi::get_max_vcpus().unwrap_or(8);
        let vcpus = cpus.min(max_vcpus).max(1) as u8;
        ffi::set_vm_config(ctx, vcpus, memory_mb)?;

        // Set rootfs: OCI/local-dir rootfs is shared via virtiofs; a
        // disk-image rootfs is attached as a block device.
        if rootfs_source == "disk_image" {
            let format = rootfs_format.unwrap_or("raw");
            ffi::set_root_disk(ctx, "root", rootfs_path, format, rootfs_read_only)?;
        } else {
            ffi::set_root(ctx, rootfs_path)?;
        }

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

        // Add disk-image mounts as block devices
        for (block_id, path, format, read_only) in disk_mounts {
            ffi::add_disk(ctx, block_id, path, format, *read_only)?;
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
        exec_agent: Option<&str>,
        rootfs_source: &str,
        rootfs_format: Option<&str>,
        rootfs_read_only: bool,
        disk_mounts: &[(String, String, String, bool)],
    ) -> std::result::Result<(), String> {
        // Create VM context
        let ctx = ffi::create_ctx()?;

        // Configure VM resources
        let max_vcpus = ffi::get_max_vcpus().unwrap_or(8);
        let vcpus = cpus.min(max_vcpus).max(1) as u8;
        ffi::set_vm_config(ctx, vcpus, memory_mb)?;

        // Set rootfs: OCI/local-dir rootfs is shared via virtiofs; a
        // disk-image rootfs is attached as a block device.
        if rootfs_source == "disk_image" {
            let format = rootfs_format.unwrap_or("raw");
            ffi::set_root_disk(ctx, "root", rootfs_path, format, rootfs_read_only)?;
        } else {
            ffi::set_root(ctx, rootfs_path)?;
        }

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

        // Add disk-image mounts as block devices
        for (block_id, path, format, read_only) in disk_mounts {
            ffi::add_disk(ctx, block_id, path, format, *read_only)?;
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
            ffi::add_vsock_port2(ctx, port, sock, true)
                .map_err(|e| format!("add_vsock_port2: {e}"))?;
        }

        // Set working directory
        if let Some(wd) = workdir {
            ffi::set_workdir(ctx, wd)?;
        }

        // The x86 kernel command line is capped at 2048 bytes and libkrun
        // exports every `KEY=VALUE` token on it as init's environment. A
        // Kubernetes container's env vars plus NANOSB_MOUNTS overflow that cap
        // (the kernel panics parsing the cmdline), so the workload environment
        // is written to the agent share and loaded by the init blob. Only the
        // compact bootstrap variables below stay on the command line.
        let env_file = extra_mounts
            .iter()
            .find(|m| m.tag == "agent")
            .map(|agent_mount| {
                let mut file_env: std::collections::BTreeMap<String, String> =
                    env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                if !dns.is_empty() {
                    file_env
                        .entry("NANOSANDBOX_DNS".to_string())
                        .or_insert_with(|| dns.join(","));
                }
                file_env
                    .entry("PATH".to_string())
                    .or_insert_with(|| {
                        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string()
                    });
                file_env
                    .entry("HOME".to_string())
                    .or_insert_with(|| "/root".to_string());
                file_env
                    .entry("TERM".to_string())
                    .or_insert_with(|| "xterm-256color".to_string());

                let host_path = std::path::Path::new(&agent_mount.host_path).join("env");
                let mut encoded = Vec::new();
                for (key, value) in &file_env {
                    encoded.extend_from_slice(key.as_bytes());
                    encoded.push(b'=');
                    encoded.extend_from_slice(value.as_bytes());
                    encoded.push(0);
                }
                std::fs::write(&host_path, &encoded)
                    .map_err(|e| format!("failed to write {}: {}", host_path.display(), e))?;
                Ok::<String, String>(format!("{}/env", agent_mount.target))
            })
            .transpose()?;

        // Build environment variables
        let mut env_vars: Vec<String> = Vec::new();
        if let Some(ref path) = env_file {
            env_vars.push(format!("NANOSB_ENV_FILE={}", path));
        } else {
            env_vars.extend(env.iter().map(|(k, v)| format!("{}={}", k, v)));
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
        }

        // Next-mode env vars for the patched init (see runtime/scripts/patches/README.md)
        env_vars.push("KRUN_NEXT_MODE=1".to_string());

        // Resident exec agent: the guest init backgrounds this binary so it
        // serves the host vsock exec channel *alongside* the PID-1 workload.
        if let (Some(agent), Some(port)) = (exec_agent, vsock_port) {
            env_vars.push(format!("KRUN_EXEC_AGENT={}:{}", agent, port));
        }

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
            std::fs::write(
                &host_config,
                serde_json::to_string(&doc).unwrap_or_default(),
            )
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
                    std::fs::create_dir_all(&host_home).map_err(|e| {
                        format!("failed to create home {}: {}", host_home.display(), e)
                    })?;
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
                ffi::add_virtio_console_default(
                    ctx,
                    cons.stdin_fd,
                    cons.stdout_fd,
                    cons.stderr_fd,
                )?;
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

        let mut rootfs_path = resolve_rootfs_path(config, bundle_path)?;

        if !rootfs_path.exists() {
            return Err(Error::SandboxCreationFailed(format!(
                "rootfs not found at {}",
                rootfs_path.display()
            )));
        }

        if config.fresh_rootfs && rootfs_path.is_dir() {
            let boot_rootfs = bundle_path
                .map(|b| b.join(format!("rootfs.boot-{}", std::process::id())))
                .unwrap_or_else(|| {
                    rootfs_path
                        .parent()
                        .unwrap_or(std::path::Path::new("."))
                        .join(format!("rootfs.boot-{}", std::process::id()))
                });
            let _ = std::fs::remove_dir_all(&boot_rootfs);
            clone_rootfs(&rootfs_path, &boot_rootfs).map_err(|e| {
                Error::SandboxCreationFailed(format!("failed to clone rootfs: {}", e))
            })?;
            rootfs_path = boot_rootfs;
        }

        // Collect mount info (canonicalize paths to prevent symlink escape / path traversal).
        // Disk-image mounts are registered as block devices; everything else is virtiofs.
        let mut mounts: Vec<(String, String)> = Vec::new();
        let mut disk_mounts: Vec<(String, String, String, bool)> = Vec::new();
        for m in &config.mounts {
            if let Some(format) = m.disk_image.as_deref() {
                let canonical = m.host_path.canonicalize().map_err(|e| {
                    Error::SandboxCreationFailed(format!(
                        "Mount path cannot be resolved: {}: {}",
                        m.host_path.display(),
                        e
                    ))
                })?;
                disk_mounts.push((
                    format!("disk{}", disk_mounts.len()),
                    canonical.to_string_lossy().to_string(),
                    format.to_string(),
                    m.readonly,
                ));
            } else if m.mount_type == MountType::VirtioFs || m.mount_type == MountType::Bind {
                let tag = format!("mount{}", mounts.len());
                let canonical = m.host_path.canonicalize().map_err(|e| {
                    Error::SandboxCreationFailed(format!(
                        "Mount path cannot be resolved: {}: {}",
                        m.host_path.display(),
                        e
                    ))
                })?;
                mounts.push((tag, canonical.to_string_lossy().to_string()));
            }
        }

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
            exec_agent: config.exec_agent.clone(),
            boot: BootConfig {
                rootfs_source: match config.rootfs.as_ref() {
                    Some(rf) if rf.source == RootfsSource::DiskImage => "disk_image".to_string(),
                    Some(rf) if rf.source == RootfsSource::LocalDir => "local_dir".to_string(),
                    _ => "oci".to_string(),
                },
                rootfs_format: config.rootfs.as_ref().and_then(|rf| rf.format.clone()),
                rootfs_read_only: config
                    .rootfs
                    .as_ref()
                    .map(|rf| rf.read_only)
                    .unwrap_or(false),
                disk_mounts,
                patches: config.patches.clone(),
                scripts: config.scripts.clone(),
                mount_specs: config.mounts.clone(),
                hostname: config.hostname.clone(),
                security_profile: config.security_profile.clone(),
                stat_virtualization: config.stat_virtualization.clone(),
                rlimits: config.rlimits.clone(),
                docker: config.docker.clone(),
            },
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
            boot,
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
                state.boot.clone(),
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
                boot,
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
                                info!("Configured DNS zone via gvproxy: {} -> {}", name, ip);
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
        exec_agent: Option<&str>,
        boot: BootConfig,
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
            exec_agent: exec_agent.map(|s| s.to_string()),
            rootfs_source: boot.rootfs_source,
            rootfs_format: boot.rootfs_format,
            rootfs_read_only: boot.rootfs_read_only,
            disk_mounts: boot.disk_mounts,
            patches: boot.patches,
            scripts: boot.scripts,
            mount_specs: boot.mount_specs,
            hostname: boot.hostname,
            security_profile: boot.security_profile,
            stat_virtualization: boot.stat_virtualization,
            rlimits: boot.rlimits,
            docker: boot.docker,
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
        boot: BootConfig,
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
            console: None,            // Set by the supervisor via env or config
            extra_mounts: Vec::new(), // Set by the supervisor via config
            env: env.clone(),
            vsock_socket: None,
            vsock_port: None,
            exec_agent: None,
            rootfs_source: boot.rootfs_source,
            rootfs_format: boot.rootfs_format,
            rootfs_read_only: boot.rootfs_read_only,
            disk_mounts: boot.disk_mounts,
            patches: boot.patches,
            scripts: boot.scripts,
            mount_specs: boot.mount_specs,
            hostname: boot.hostname,
            security_profile: boot.security_profile,
            stat_virtualization: boot.stat_virtualization,
            rlimits: boot.rlimits,
            docker: boot.docker,
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
            exec_agent,
            boot,
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
                state.exec_agent.clone(),
                state.boot.clone(),
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
                exec_agent.as_deref(),
                boot,
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
                                info!("Configured DNS zone via gvproxy: {} -> {}", name, ip);
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

        info!(
            "VM (next mode) started for sandbox '{}' (pid: {})",
            id, vm_pid
        );
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

            if state
                .rootfs_path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rootfs.boot-"))
            {
                let _ = std::fs::remove_dir_all(&state.rootfs_path);
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

/// Resolve the rootfs directory for a sandbox: the configured non-OCI rootfs
/// (local dir / disk image) when present, otherwise `<bundle>/rootfs`. A typed
/// error is returned when neither is available.
fn resolve_rootfs_path(config: &SandboxConfig, bundle_path: Option<&Path>) -> Result<PathBuf> {
    match config.rootfs.as_ref() {
        Some(rf) if rf.source == RootfsSource::LocalDir || rf.source == RootfsSource::DiskImage => {
            Ok(PathBuf::from(rf.path.as_deref().ok_or_else(|| {
                Error::SandboxCreationFailed(format!(
                    "rootfs source {:?} requires a path",
                    rf.source
                ))
            })?))
        }
        _ => {
            let bundle = bundle_path.ok_or_else(|| {
                Error::SandboxCreationFailed(
                    "LibkrunRuntime requires a bundle path with rootfs (handles_image_pull=false)"
                        .to_string(),
                )
            })?;
            Ok(bundle.join("rootfs"))
        }
    }
}

#[cfg(test)]
mod rootfs_resolution_tests {
    use super::*;
    use crate::config::RootfsConfig;

    fn cfg(source: RootfsSource, path: Option<&str>) -> SandboxConfig {
        let mut c = SandboxConfig::default();
        c.rootfs = Some(RootfsConfig {
            source,
            path: path.map(str::to_string),
            fstype: None,
            format: None,
            read_only: false,
        });
        c
    }

    #[test]
    fn local_dir_rootfs_needs_no_bundle() {
        let c = cfg(RootfsSource::LocalDir, Some("/tmp/rootfs-dir"));
        assert_eq!(
            resolve_rootfs_path(&c, None).unwrap(),
            PathBuf::from("/tmp/rootfs-dir")
        );
    }

    #[test]
    fn disk_image_rootfs_needs_no_bundle() {
        let c = cfg(RootfsSource::DiskImage, Some("/tmp/rootfs.img"));
        assert_eq!(
            resolve_rootfs_path(&c, None).unwrap(),
            PathBuf::from("/tmp/rootfs.img")
        );
    }

    #[test]
    fn non_oci_rootfs_without_path_is_typed_error() {
        let c = cfg(RootfsSource::LocalDir, None);
        assert!(resolve_rootfs_path(&c, None).is_err());
    }

    #[test]
    fn oci_rootfs_uses_bundle_rootfs() {
        let c = cfg(RootfsSource::Oci, None);
        assert_eq!(
            resolve_rootfs_path(&c, Some(Path::new("/bundle"))).unwrap(),
            PathBuf::from("/bundle/rootfs")
        );
    }

    #[test]
    fn oci_rootfs_without_bundle_is_typed_error() {
        let c = cfg(RootfsSource::Oci, None);
        let err = resolve_rootfs_path(&c, None).unwrap_err();
        assert!(err.to_string().contains("requires a bundle path"), "{err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_rootfs(passwd: &str, group: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nanosb-usr-{}-{}",
            std::process::id(),
            unique_nonce()
        ));
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

    const PASSWD: &str =
        "root:x:0:0:root:/root:/bin/sh\ndeveloper:x:1000:1000:Dev:/home/developer:/bin/bash\n";
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
        let dir = std::env::temp_dir().join(format!(
            "nanosb-nopasswd-{}-{}",
            std::process::id(),
            unique_nonce()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let err = resolve_guest_user(&dir, "developer", None).unwrap_err();
        assert!(err.contains("/etc/passwd"), "{}", err);
        assert!(err.contains("numeric uid"), "{}", err);
    }

    #[test]
    fn numeric_uid_without_passwd_still_works() {
        let dir = std::env::temp_dir().join(format!(
            "nanosb-nopasswd2-{}-{}",
            std::process::id(),
            unique_nonce()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let (uid, gid, home) = resolve_guest_user(&dir, "1000", None).unwrap();
        assert_eq!(uid, "1000");
        assert_eq!(gid, None);
        assert_eq!(home, None);
    }

    #[test]
    fn clone_rootfs_copies_files_and_symlinks() {
        let base = std::env::temp_dir().join(format!(
            "nanosb-clone-{}-{}",
            std::process::id(),
            unique_nonce()
        ));
        let src = base.join("src");
        let dst = base.join("dst");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("f.txt"), b"hi").unwrap();
        std::fs::write(src.join("sub/g.txt"), b"there").unwrap();
        std::os::unix::fs::symlink("f.txt", src.join("link")).unwrap();

        clone_rootfs(&src, &dst).unwrap();

        assert_eq!(std::fs::read(dst.join("f.txt")).unwrap(), b"hi");
        assert_eq!(std::fs::read(dst.join("sub/g.txt")).unwrap(), b"there");
        assert!(std::fs::symlink_metadata(dst.join("link"))
            .unwrap()
            .file_type()
            .is_symlink());
        let _ = std::fs::remove_dir_all(&base);
    }

    fn fixture_rootfs_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nanosb-patch-{}-{}",
            std::process::id(),
            unique_nonce()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn apply_patches_mkdir_text_copy_remove() {
        let root = fixture_rootfs_dir();
        let host_file = root.join("host-file.txt");
        std::fs::write(&host_file, b"from-host").unwrap();
        let host_dir = root.join("host-dir");
        std::fs::create_dir_all(&host_dir).unwrap();
        std::fs::write(host_dir.join("a.txt"), b"a").unwrap();

        let patches = vec![
            RootfsPatchConfig::Mkdir {
                path: "/etc/nanosb".to_string(),
                mode: Some(0o750),
            },
            RootfsPatchConfig::Text {
                path: "/etc/nanosb/hello.txt".to_string(),
                content: "hello".to_string(),
                mode: Some(0o600),
            },
            RootfsPatchConfig::CopyFile {
                from: host_file.to_string_lossy().to_string(),
                to: "/opt/copied.txt".to_string(),
                mode: None,
            },
            RootfsPatchConfig::CopyDir {
                from: host_dir.to_string_lossy().to_string(),
                to: "/opt/host-dir".to_string(),
            },
        ];
        apply_rootfs_patches(&root, &patches).unwrap();

        assert_eq!(
            std::fs::read(root.join("etc/nanosb/hello.txt")).unwrap(),
            b"hello"
        );
        assert_eq!(
            std::fs::read(root.join("opt/copied.txt")).unwrap(),
            b"from-host"
        );
        assert_eq!(
            std::fs::read(root.join("opt/host-dir/a.txt")).unwrap(),
            b"a"
        );
        assert!(root.join("etc/nanosb").is_dir());

        apply_rootfs_patches(
            &root,
            &vec![RootfsPatchConfig::Remove {
                path: "/opt/copied.txt".to_string(),
                recursive: false,
            }],
        )
        .unwrap();
        assert!(!root.join("opt/copied.txt").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn apply_patches_rejects_path_escape() {
        let root = fixture_rootfs_dir();
        let err = apply_rootfs_patches(
            &root,
            &vec![RootfsPatchConfig::Text {
                path: "../escape.txt".to_string(),
                content: "x".to_string(),
                mode: None,
            }],
        )
        .unwrap_err();
        assert!(err.contains("escapes rootfs"), "{}", err);
        assert!(!root.parent().unwrap().join("escape.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn materialize_scripts_writes_executable() {
        use std::os::unix::fs::PermissionsExt;
        let root = fixture_rootfs_dir();
        let scripts = HashMap::from([("run.sh".to_string(), "echo hi".to_string())]);
        materialize_scripts(&root, &scripts).unwrap();
        let path = root.join(".nanosb/scripts/run.sh");
        assert_eq!(std::fs::read(&path).unwrap(), b"echo hi");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn materialize_scripts_rejects_path_traversal_name() {
        let root = fixture_rootfs_dir();
        let scripts = HashMap::from([("../evil".to_string(), "x".to_string())]);
        let err = materialize_scripts(&root, &scripts).unwrap_err();
        assert!(err.contains("invalid script name"), "{}", err);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn mounts_env_json_serializes_kinds_and_tags() {
        let mounts = vec![
            Mount::virtiofs("/host/ws".to_string(), "/workspace".to_string()),
            Mount::tmpfs("/dev/shm".to_string()),
            Mount {
                host_path: std::path::PathBuf::from("/host/secret"),
                container_path: "/etc/secret".to_string(),
                readonly: true,
                mount_type: MountType::Bind,
                file: true,
                disk_image: None,
                fstype: None,
                options: vec!["noexec".to_string()],
                uid: Some(1000),
                gid: None,
            },
            Mount {
                host_path: std::path::PathBuf::from("/host/data.img"),
                container_path: "/mnt/data".to_string(),
                readonly: false,
                mount_type: MountType::Bind,
                file: false,
                disk_image: Some("qcow2".to_string()),
                fstype: Some("ext4".to_string()),
                options: Vec::new(),
                uid: None,
                gid: None,
            },
        ];
        let value = mounts_env_json(&mounts);
        let json = serde_json::to_string(&value).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.len(), 4);
        let first = parsed[0].as_object().unwrap();
        assert_eq!(first.get("target").unwrap().as_str().unwrap(), "/workspace");
        assert_eq!(first.get("kind").unwrap().as_str().unwrap(), "virtiofs");
        assert_eq!(first.get("tag").unwrap().as_str().unwrap(), "mount0");
        assert_eq!(first.get("readonly").unwrap().as_bool().unwrap(), false);

        let tmpfs = parsed[1].as_object().unwrap();
        assert_eq!(tmpfs.get("kind").unwrap().as_str().unwrap(), "tmpfs");
        assert!(tmpfs.get("source").is_none());

        let file = parsed[2].as_object().unwrap();
        assert_eq!(file.get("kind").unwrap().as_str().unwrap(), "file");
        assert_eq!(file.get("tag").unwrap().as_str().unwrap(), "mount1");
        assert_eq!(file.get("readonly").unwrap().as_bool().unwrap(), true);
        assert_eq!(file.get("uid").unwrap().as_u64().unwrap(), 1000);
        assert_eq!(
            file.get("options").unwrap().as_array().unwrap()[0]
                .as_str()
                .unwrap(),
            "noexec"
        );

        let disk = parsed[3].as_object().unwrap();
        assert_eq!(disk.get("kind").unwrap().as_str().unwrap(), "disk");
        assert_eq!(disk.get("tag").unwrap().as_str().unwrap(), "disk0");
        assert_eq!(disk.get("fstype").unwrap().as_str().unwrap(), "ext4");
    }

    #[test]
    fn docker_guest_env_none_when_docker_absent() {
        assert!(docker_guest_env(None).is_none());
    }

    #[test]
    fn docker_guest_env_json_when_docker_present() {
        let docker = DockerConfig {
            enabled: true,
            runtime: crate::config::DockerRuntime::Podman,
            storage_driver: crate::config::DockerStorageDriver::FuseOverlayfs,
            data_size_bytes: 42 * 1024 * 1024 * 1024,
            network: crate::config::DockerNetwork::Bridge,
            daemon_json: HashMap::from([(
                "log-driver".to_string(),
                serde_json::Value::String("json-file".to_string()),
            )]),
            preload_images: vec!["busybox:latest".to_string()],
            rootless: true,
            socket: "/run/docker.sock".to_string(),
        };

        let (key, json) = docker_guest_env(Some(&docker)).expect("docker env pair");
        assert_eq!(key, "NANOSB_DOCKER");
        let back: DockerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, docker);
    }

    #[test]
    fn docker_host_env_none_when_disabled() {
        let docker = DockerConfig {
            enabled: false,
            ..Default::default()
        };
        assert!(docker_host_env(Some(&docker)).is_none());
    }

    #[test]
    fn docker_host_env_none_when_absent() {
        assert!(docker_host_env(None).is_none());
    }

    #[test]
    fn docker_host_env_set_when_enabled() {
        let docker = DockerConfig {
            enabled: true,
            socket: "/run/docker.sock".to_string(),
            ..Default::default()
        };
        let (key, value) = docker_host_env(Some(&docker)).expect("docker host env pair");
        assert_eq!(key, "DOCKER_HOST");
        assert_eq!(value, "unix:///run/docker.sock");
    }
}
