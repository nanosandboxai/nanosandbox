//! FFI bindings for libkrun (statically linked via libkrun-sys)
//!
//! Thin delegation layer to libkrun-sys safe wrappers. libkrun is compiled
//! directly into the nanosandbox binary as an rlib — no dlopen/libloading.
//!
//! Networking modes:
//! - **gvproxy (preferred)**: When `add_net_unixgram` is called with a
//!   gvproxy socket, a virtio-net device provides full outbound networking.
//! - **TSI (fallback)**: When no network device is added, libkrun automatically
//!   enables Transparent Socket Impersonation (limited outbound connectivity).

// ---------------------------------------------------------------------------
// TSI feature flags (from libkrun.h)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub const KRUN_TSI_HIJACK_INET: u32 = 1 << 0;
#[allow(dead_code)]
pub const KRUN_TSI_HIJACK_UNIX: u32 = 1 << 1;

// ---------------------------------------------------------------------------
// Logging constants (from libkrun.h)
// ---------------------------------------------------------------------------

pub const KRUN_LOG_TARGET_DEFAULT: i32 = -1;
#[allow(dead_code)]
pub const KRUN_LOG_LEVEL_OFF: u32 = 0;
#[allow(dead_code)]
pub const KRUN_LOG_LEVEL_ERROR: u32 = 1;
pub const KRUN_LOG_LEVEL_WARN: u32 = 2;
#[allow(dead_code)]
pub const KRUN_LOG_LEVEL_INFO: u32 = 3;
#[allow(dead_code)]
pub const KRUN_LOG_LEVEL_DEBUG: u32 = 4;
#[allow(dead_code)]
pub const KRUN_LOG_LEVEL_TRACE: u32 = 5;
#[allow(dead_code)]
pub const KRUN_LOG_STYLE_NEVER: u32 = 2;
#[allow(dead_code)]
pub const KRUN_LOG_OPTION_NO_ENV: u32 = 1;

// ---------------------------------------------------------------------------
// virtio-net / gvproxy constants
// ---------------------------------------------------------------------------

pub const NET_FLAG_VFKIT: u32 = 1 << 0;

const NET_FEATURE_CSUM: u32 = 1 << 0;
const NET_FEATURE_GUEST_CSUM: u32 = 1 << 1;
const NET_FEATURE_GUEST_TSO4: u32 = 1 << 7;
const NET_FEATURE_GUEST_UFO: u32 = 1 << 10;
const NET_FEATURE_HOST_TSO4: u32 = 1 << 11;
const NET_FEATURE_HOST_UFO: u32 = 1 << 14;

pub const COMPAT_NET_FEATURES: u32 = NET_FEATURE_CSUM
    | NET_FEATURE_GUEST_CSUM
    | NET_FEATURE_GUEST_TSO4
    | NET_FEATURE_GUEST_UFO
    | NET_FEATURE_HOST_TSO4
    | NET_FEATURE_HOST_UFO;

pub const GVPROXY_GUEST_MAC: [u8; 6] = [0x5a, 0x94, 0xef, 0xe4, 0x0c, 0xee];

// ---------------------------------------------------------------------------
// Feature constants
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub const KRUN_FEATURE_NET: u64 = 0;
#[allow(dead_code)]
pub const KRUN_FEATURE_BLK: u64 = 1;
#[allow(dead_code)]
pub const KRUN_FEATURE_GPU: u64 = 2;
#[allow(dead_code)]
pub const KRUN_FEATURE_SND: u64 = 3;

// ---------------------------------------------------------------------------
// Safe wrappers — delegate to libkrun-sys
// ---------------------------------------------------------------------------

/// Safe wrapper around `krun_set_log_level`.
///
/// Uses env_logger probe to avoid double-init panic.
pub fn set_log_level(level: u32) -> Result<(), String> {
    let env_filter = match level {
        0 => "off",
        1 => "error",
        2 => "warn",
        3 => "info",
        _ => "debug",
    };
    let default_env = env_logger::Env::default().default_filter_or(env_filter);
    let _ = env_logger::try_init_from_env(default_env);
    Ok(())
}

pub fn init_log(target_fd: i32, level: u32) -> Result<(), String> {
    libkrun_sys::safe_init_log(target_fd, level)
}

pub fn create_ctx() -> Result<u32, String> {
    libkrun_sys::safe_create_ctx()
}

#[allow(dead_code)]
pub fn free_ctx(ctx_id: u32) -> Result<(), String> {
    libkrun_sys::safe_free_ctx(ctx_id)
}

pub fn set_vm_config(ctx_id: u32, num_vcpus: u8, ram_mib: u32) -> Result<(), String> {
    libkrun_sys::safe_set_vm_config(ctx_id, num_vcpus, ram_mib)
}

pub fn set_root(ctx_id: u32, root_path: &str) -> Result<(), String> {
    libkrun_sys::safe_set_root(ctx_id, root_path)
}

pub fn add_virtiofs(ctx_id: u32, tag: &str, path: &str) -> Result<(), String> {
    libkrun_sys::safe_add_virtiofs(ctx_id, tag, path)
}

pub fn set_port_map(ctx_id: u32, mappings: Option<&[String]>) -> Result<(), String> {
    libkrun_sys::safe_set_port_map(ctx_id, mappings)
}

pub fn set_workdir(ctx_id: u32, workdir: &str) -> Result<(), String> {
    libkrun_sys::safe_set_workdir(ctx_id, workdir)
}

pub fn set_exec(
    ctx_id: u32,
    exec_path: &str,
    argv: &[&str],
    envp: Option<&[String]>,
) -> Result<(), String> {
    libkrun_sys::safe_set_exec(ctx_id, exec_path, argv, envp)
}

#[allow(dead_code)]
pub fn set_env(ctx_id: u32, env_vars: &[String]) -> Result<(), String> {
    libkrun_sys::safe_set_env(ctx_id, env_vars)
}

#[allow(dead_code)]
pub fn set_console_output(ctx_id: u32, filepath: &str) -> Result<(), String> {
    libkrun_sys::safe_set_console_output(ctx_id, filepath)
}

pub fn disable_implicit_console(ctx_id: u32) -> Result<(), String> {
    libkrun_sys::safe_disable_implicit_console(ctx_id)
}

pub fn add_virtio_console_default(
    ctx_id: u32,
    input_fd: i32,
    output_fd: i32,
    err_fd: i32,
) -> Result<(), String> {
    libkrun_sys::safe_add_virtio_console_default(ctx_id, input_fd, output_fd, err_fd)
}

pub fn add_virtio_console_multiport(ctx_id: u32) -> Result<u32, String> {
    libkrun_sys::safe_add_virtio_console_multiport(ctx_id)
}

pub fn add_console_port_tty(
    ctx_id: u32,
    console_id: u32,
    name: &str,
    tty_fd: i32,
) -> Result<(), String> {
    libkrun_sys::safe_add_console_port_tty(ctx_id, console_id, name, tty_fd)
}

#[allow(dead_code)]
pub fn add_vsock(ctx_id: u32, tsi_features: u32) -> Result<(), String> {
    libkrun_sys::safe_add_vsock(ctx_id, tsi_features)
}

#[allow(dead_code)]
pub fn disable_implicit_vsock(ctx_id: u32) -> Result<(), String> {
    libkrun_sys::safe_disable_implicit_vsock(ctx_id)
}

pub fn add_net_unixgram(
    ctx_id: u32,
    socket_path: &str,
    mac: &[u8; 6],
    features: u32,
    flags: u32,
) -> Result<(), String> {
    libkrun_sys::safe_add_net_unixgram(ctx_id, socket_path, mac, features, flags)
}

pub fn start_enter(ctx_id: u32) -> Result<(), String> {
    libkrun_sys::safe_start_enter(ctx_id)
}

#[cfg(unix)]
pub fn get_max_vcpus() -> Result<u32, String> {
    libkrun_sys::safe_get_max_vcpus()
}

#[allow(dead_code)]
pub fn has_feature(feature: u64) -> Result<bool, String> {
    libkrun_sys::safe_has_feature(feature)
}

#[allow(dead_code)]
pub fn add_virtiofs2(ctx_id: u32, tag: &str, path: &str, shm_size: u64) -> Result<(), String> {
    libkrun_sys::safe_add_virtiofs2(ctx_id, tag, path, shm_size)
}

#[allow(dead_code)]
pub fn add_net_unixstream(
    ctx_id: u32,
    socket_path: &str,
    mac: &[u8; 6],
    features: u32,
    flags: u32,
) -> Result<(), String> {
    libkrun_sys::safe_add_net_unixstream(ctx_id, socket_path, mac, features, flags)
}

#[allow(dead_code)]
pub fn add_vsock_port(ctx_id: u32, port: u32, filepath: &str) -> Result<(), String> {
    libkrun_sys::safe_add_vsock_port(ctx_id, port, filepath)
}

#[allow(dead_code)]
pub fn add_vsock_port2(ctx_id: u32, port: u32, filepath: &str, listen: bool) -> Result<(), String> {
    libkrun_sys::safe_add_vsock_port2(ctx_id, port, filepath, listen)
}

#[allow(dead_code)]
pub fn set_rlimits(ctx_id: u32, rlimits: &[String]) -> Result<(), String> {
    libkrun_sys::safe_set_rlimits(ctx_id, rlimits)
}

#[allow(dead_code)]
pub fn set_smbios_oem_strings(ctx_id: u32, strings: &[String]) -> Result<(), String> {
    libkrun_sys::safe_set_smbios_oem_strings(ctx_id, strings)
}

