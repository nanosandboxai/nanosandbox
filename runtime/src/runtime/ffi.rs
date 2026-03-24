//! FFI bindings for libkrun (runtime-loaded via dlopen)
//!
//! Direct Rust FFI declarations for the libkrun C API, loaded at runtime
//! via `libloading` so the binary can start without libkrun installed.
//! This allows the auto-setup module to detect and install libkrun before
//! any VM operations are attempted.
//!
//! Networking modes:
//! - **gvproxy (preferred)**: When `krun_add_net_unixgram` is called with a
//!   gvproxy socket, a virtio-net device provides full outbound networking.
//! - **TSI (fallback)**: When no network device is added, libkrun automatically
//!   enables Transparent Socket Impersonation (limited outbound connectivity).
//!
//! Reference: <https://github.com/containers/libkrun>
//! Header:    <https://github.com/containers/libkrun/blob/main/include/libkrun.h>

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_uint};
use std::sync::Mutex;

use libloading::Library;

// ---------------------------------------------------------------------------
// Runtime-loaded libkrun library
// ---------------------------------------------------------------------------

/// The loaded libkrun library handle, initialized on first FFI call.
/// Uses Mutex<Option<Library>> since OnceLock::get_or_try_init is unstable.
static LIBKRUN: Mutex<Option<Library>> = Mutex::new(None);

/// Find and load libkrun at runtime.
///
/// Search order:
/// - macOS: /opt/homebrew/lib/libkrun.dylib, /usr/local/lib/libkrun.dylib
/// - Linux: libkrun.so (system linker paths), then explicit paths
/// - Windows: krun.dll (current dir, PATH, system dirs)
fn load_libkrun() -> Result<Library, String> {
    #[cfg(target_os = "macos")]
    let candidates = &[
        "/opt/homebrew/lib/libkrun.dylib",
        "/usr/local/lib/libkrun.dylib",
    ];

    #[cfg(target_os = "linux")]
    let candidates = &[
        "libkrun.so",
        "/usr/lib/libkrun.so",
        "/usr/lib64/libkrun.so",
        "/usr/local/lib/libkrun.so",
        "/usr/lib/x86_64-linux-gnu/libkrun.so",
        "/usr/lib/aarch64-linux-gnu/libkrun.so",
    ];

    #[cfg(target_os = "windows")]
    let candidates = &[
        "krun.dll",
        "C:\\libkrun-win\\target\\release\\krun.dll",
        "C:\\libkrun-win\\target\\debug\\krun.dll",
    ];

    let mut last_err = String::new();
    for path in candidates {
        match unsafe { Library::new(path) } {
            Ok(lib) => return Ok(lib),
            Err(e) => last_err = format!("{}: {}", path, e),
        }
    }

    Err(format!(
        "libkrun not found. Install it first.\nLast error: {}",
        last_err
    ))
}

/// Ensure libkrun is loaded and call a function with it.
///
/// The library is loaded once and cached in the static Mutex.
/// This gives us interior mutability for init while keeping
/// the library alive for the process lifetime.
fn with_lib<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce(&Library) -> Result<R, String>,
{
    let mut guard = LIBKRUN.lock().map_err(|e| format!("lock poisoned: {}", e))?;
    if guard.is_none() {
        *guard = Some(load_libkrun()?);
    }
    f(guard.as_ref().unwrap())
}

// TSI feature flags (from libkrun.h)
#[allow(dead_code)]
pub const KRUN_TSI_HIJACK_INET: u32 = 1 << 0;
#[allow(dead_code)]
pub const KRUN_TSI_HIJACK_UNIX: u32 = 1 << 1;

// Logging constants (from libkrun.h)
/// Use default log target (stderr)
#[allow(dead_code)]
pub const KRUN_LOG_TARGET_DEFAULT: i32 = -1;
/// Log levels
#[allow(dead_code)]
pub const KRUN_LOG_LEVEL_OFF: u32 = 0;
#[allow(dead_code)]
pub const KRUN_LOG_LEVEL_ERROR: u32 = 1;
#[allow(dead_code)]
pub const KRUN_LOG_LEVEL_WARN: u32 = 2;
#[allow(dead_code)]
pub const KRUN_LOG_LEVEL_INFO: u32 = 3;
#[allow(dead_code)]
pub const KRUN_LOG_LEVEL_DEBUG: u32 = 4;
#[allow(dead_code)]
pub const KRUN_LOG_LEVEL_TRACE: u32 = 5;
/// Log style: no terminal escape sequences
#[allow(dead_code)]
pub const KRUN_LOG_STYLE_NEVER: u32 = 2;
/// Prevent environment variables from overriding log settings
#[allow(dead_code)]
pub const KRUN_LOG_OPTION_NO_ENV: u32 = 1;

// ---------------------------------------------------------------------------
// virtio-net / gvproxy constants (from libkrun.h / krunkit)
// ---------------------------------------------------------------------------

/// Send the VFKIT magic after establishing the connection,
/// as required by gvproxy in vfkit mode.
#[allow(dead_code)]
pub const NET_FLAG_VFKIT: u32 = 1 << 0;

/// virtio-net feature flags for offloading (matching krunkit's COMPAT_NET_FEATURES).
#[allow(dead_code)]
const NET_FEATURE_CSUM: u32 = 1 << 0;
#[allow(dead_code)]
const NET_FEATURE_GUEST_CSUM: u32 = 1 << 1;
#[allow(dead_code)]
const NET_FEATURE_GUEST_TSO4: u32 = 1 << 7;
#[allow(dead_code)]
const NET_FEATURE_GUEST_UFO: u32 = 1 << 10;
#[allow(dead_code)]
const NET_FEATURE_HOST_TSO4: u32 = 1 << 11;
#[allow(dead_code)]
const NET_FEATURE_HOST_UFO: u32 = 1 << 14;

#[allow(dead_code)]
pub const COMPAT_NET_FEATURES: u32 = NET_FEATURE_CSUM
    | NET_FEATURE_GUEST_CSUM
    | NET_FEATURE_GUEST_TSO4
    | NET_FEATURE_GUEST_UFO
    | NET_FEATURE_HOST_TSO4
    | NET_FEATURE_HOST_UFO;

/// Default guest MAC address (matches gvproxy's default DHCP static lease).
#[allow(dead_code)]
pub const GVPROXY_GUEST_MAC: [u8; 6] = [0x5a, 0x94, 0xef, 0xe4, 0x0c, 0xee];

// ---------------------------------------------------------------------------
// Safe Rust wrappers
// ---------------------------------------------------------------------------

/// Convert a Rust string to a CString, returning None if conversion fails.
fn to_cstring(s: &str) -> Option<CString> {
    CString::new(s).ok()
}

/// Convert a slice of strings into a null-terminated C array of pointers.
/// The caller must keep the returned `Vec<CString>` alive while using the pointer array.
fn strings_to_c_array(strings: &[&str]) -> (Vec<CString>, Vec<*const c_char>) {
    let c_strings: Vec<CString> = strings
        .iter()
        .filter_map(|s| to_cstring(s))
        .collect();
    let mut ptrs: Vec<*const c_char> = c_strings.iter().map(|cs| cs.as_ptr()).collect();
    ptrs.push(std::ptr::null()); // null terminator
    (c_strings, ptrs)
}

/// Convert a slice of owned Strings into a null-terminated C array of pointers.
fn owned_strings_to_c_array(strings: &[String]) -> (Vec<CString>, Vec<*const c_char>) {
    let c_strings: Vec<CString> = strings
        .iter()
        .filter_map(|s| to_cstring(s))
        .collect();
    let mut ptrs: Vec<*const c_char> = c_strings.iter().map(|cs| cs.as_ptr()).collect();
    ptrs.push(std::ptr::null()); // null terminator
    (c_strings, ptrs)
}

/// Safe wrapper around `krun_set_log_level`.
///
/// **Important**: `krun_set_log_level` internally calls `env_logger::Builder::init()`,
/// which aborts the process if a global logger is already registered.
///
/// We probe with `env_logger::try_init_from_env()` first to avoid double-init.
pub fn set_log_level(level: u32) -> Result<(), String> {
    let env_filter = match level {
        0 => "off",
        1 => "error",
        2 => "warn",
        3 => "info",
        _ => "debug",
    };

    let default_env = env_logger::Env::default().default_filter_or(env_filter);
    if env_logger::try_init_from_env(default_env).is_ok() {
        return Ok(());
    }

    Ok(())
}

/// Initialize libkrun logging via `krun_init_log`.
///
/// Unlike `set_log_level` (which uses env_logger and has conflict issues),
/// this API writes directly to a file descriptor. Safe to call in forked
/// child processes regardless of the parent's logger state.
pub fn init_log(target_fd: i32, level: u32) -> Result<(), String> {
    with_lib(|lib| {
        let func: libloading::Symbol<unsafe extern "C" fn(c_int, c_uint, c_uint, c_uint) -> c_int> =
            unsafe { lib.get(b"krun_init_log") }.map_err(|e| format!("krun_init_log: {}", e))?;
        let ret = unsafe { func(target_fd as c_int, level as c_uint, KRUN_LOG_STYLE_NEVER as c_uint, KRUN_LOG_OPTION_NO_ENV as c_uint) };
        if ret < 0 { Err(format!("krun_init_log failed with code {}", ret)) } else { Ok(()) }
    })
}

/// Safe wrapper around `krun_create_ctx`. Returns the context ID.
pub fn create_ctx() -> Result<u32, String> {
    with_lib(|lib| {
        let func: libloading::Symbol<unsafe extern "C" fn() -> c_int> =
            unsafe { lib.get(b"krun_create_ctx") }.map_err(|e| format!("krun_create_ctx: {}", e))?;
        let ret = unsafe { func() };
        if ret < 0 { Err(format!("krun_create_ctx failed with code {}", ret)) } else { Ok(ret as u32) }
    })
}

/// Safe wrapper around `krun_free_ctx`.
#[allow(dead_code)]
pub fn free_ctx(ctx_id: u32) -> Result<(), String> {
    with_lib(|lib| {
        let func: libloading::Symbol<unsafe extern "C" fn(c_uint) -> c_int> =
            unsafe { lib.get(b"krun_free_ctx") }.map_err(|e| format!("krun_free_ctx: {}", e))?;
        let ret = unsafe { func(ctx_id as c_uint) };
        if ret < 0 { Err(format!("krun_free_ctx failed with code {}", ret)) } else { Ok(()) }
    })
}

/// Safe wrapper around `krun_set_vm_config`.
pub fn set_vm_config(ctx_id: u32, num_vcpus: u8, ram_mib: u32) -> Result<(), String> {
    with_lib(|lib| {
        let func: libloading::Symbol<unsafe extern "C" fn(c_uint, u8, c_uint) -> c_int> =
            unsafe { lib.get(b"krun_set_vm_config") }.map_err(|e| format!("krun_set_vm_config: {}", e))?;
        let ret = unsafe { func(ctx_id as c_uint, num_vcpus, ram_mib as c_uint) };
        if ret < 0 { Err(format!("krun_set_vm_config failed with code {}", ret)) } else { Ok(()) }
    })
}

/// Safe wrapper around `krun_set_root`.
pub fn set_root(ctx_id: u32, root_path: &str) -> Result<(), String> {
    with_lib(|lib| {
        let func: libloading::Symbol<unsafe extern "C" fn(c_uint, *const c_char) -> c_int> =
            unsafe { lib.get(b"krun_set_root") }.map_err(|e| format!("krun_set_root: {}", e))?;
        let c_path = to_cstring(root_path).ok_or_else(|| "Invalid root path".to_string())?;
        let ret = unsafe { func(ctx_id as c_uint, c_path.as_ptr()) };
        if ret < 0 { Err(format!("krun_set_root failed with code {}", ret)) } else { Ok(()) }
    })
}

/// Safe wrapper around `krun_add_virtiofs`.
pub fn add_virtiofs(ctx_id: u32, tag: &str, path: &str) -> Result<(), String> {
    with_lib(|lib| {
        let func: libloading::Symbol<unsafe extern "C" fn(c_uint, *const c_char, *const c_char) -> c_int> =
            unsafe { lib.get(b"krun_add_virtiofs") }.map_err(|e| format!("krun_add_virtiofs: {}", e))?;
        let c_tag = to_cstring(tag).ok_or_else(|| "Invalid virtiofs tag".to_string())?;
        let c_path = to_cstring(path).ok_or_else(|| "Invalid virtiofs path".to_string())?;
        let ret = unsafe { func(ctx_id as c_uint, c_tag.as_ptr(), c_path.as_ptr()) };
        if ret < 0 { Err(format!("krun_add_virtiofs failed with code {}", ret)) } else { Ok(()) }
    })
}

/// Safe wrapper around `krun_set_port_map`.
pub fn set_port_map(ctx_id: u32, mappings: Option<&[String]>) -> Result<(), String> {
    with_lib(|lib| {
        let func: libloading::Symbol<unsafe extern "C" fn(c_uint, *const *const c_char) -> c_int> =
            unsafe { lib.get(b"krun_set_port_map") }.map_err(|e| format!("krun_set_port_map: {}", e))?;
        match mappings {
            None => {
                let ret = unsafe { func(ctx_id as c_uint, std::ptr::null()) };
                if ret < 0 { Err(format!("krun_set_port_map(NULL) failed with code {}", ret)) } else { Ok(()) }
            }
            Some(maps) => {
                let (_c_strings, ptrs) = owned_strings_to_c_array(maps);
                let ret = unsafe { func(ctx_id as c_uint, ptrs.as_ptr()) };
                if ret < 0 { Err(format!("krun_set_port_map failed with code {}", ret)) } else { Ok(()) }
            }
        }
    })
}

/// Safe wrapper around `krun_set_workdir`.
pub fn set_workdir(ctx_id: u32, workdir: &str) -> Result<(), String> {
    with_lib(|lib| {
        let func: libloading::Symbol<unsafe extern "C" fn(c_uint, *const c_char) -> c_int> =
            unsafe { lib.get(b"krun_set_workdir") }.map_err(|e| format!("krun_set_workdir: {}", e))?;
        let c_workdir = to_cstring(workdir).ok_or_else(|| "Invalid workdir".to_string())?;
        let ret = unsafe { func(ctx_id as c_uint, c_workdir.as_ptr()) };
        if ret < 0 { Err(format!("krun_set_workdir failed with code {}", ret)) } else { Ok(()) }
    })
}

/// Safe wrapper around `krun_set_exec`.
pub fn set_exec(ctx_id: u32, exec_path: &str, argv: &[&str], envp: Option<&[String]>) -> Result<(), String> {
    with_lib(|lib| {
        let func: libloading::Symbol<unsafe extern "C" fn(c_uint, *const c_char, *const *const c_char, *const *const c_char) -> c_int> =
            unsafe { lib.get(b"krun_set_exec") }.map_err(|e| format!("krun_set_exec: {}", e))?;
        let c_exec = to_cstring(exec_path).ok_or_else(|| "Invalid exec path".to_string())?;
        let (_c_argv, argv_ptrs) = strings_to_c_array(argv);
        let ret = match envp {
            Some(env_vars) => {
                let (_c_env, env_ptrs) = owned_strings_to_c_array(env_vars);
                unsafe { func(ctx_id as c_uint, c_exec.as_ptr(), argv_ptrs.as_ptr(), env_ptrs.as_ptr()) }
            }
            None => {
                unsafe { func(ctx_id as c_uint, c_exec.as_ptr(), argv_ptrs.as_ptr(), std::ptr::null()) }
            }
        };
        if ret < 0 { Err(format!("krun_set_exec failed with code {}", ret)) } else { Ok(()) }
    })
}

/// Safe wrapper around `krun_set_env`.
#[allow(dead_code)]
pub fn set_env(ctx_id: u32, env_vars: &[String]) -> Result<(), String> {
    with_lib(|lib| {
        let func: libloading::Symbol<unsafe extern "C" fn(c_uint, *const *const c_char) -> c_int> =
            unsafe { lib.get(b"krun_set_env") }.map_err(|e| format!("krun_set_env: {}", e))?;
        let (_c_strings, ptrs) = owned_strings_to_c_array(env_vars);
        let ret = unsafe { func(ctx_id as c_uint, ptrs.as_ptr()) };
        if ret < 0 { Err(format!("krun_set_env failed with code {}", ret)) } else { Ok(()) }
    })
}

/// Safe wrapper around `krun_set_console_output`.
#[allow(dead_code)]
pub fn set_console_output(ctx_id: u32, filepath: &str) -> Result<(), String> {
    with_lib(|lib| {
        let func: libloading::Symbol<unsafe extern "C" fn(c_uint, *const c_char) -> c_int> =
            unsafe { lib.get(b"krun_set_console_output") }.map_err(|e| format!("krun_set_console_output: {}", e))?;
        let c_path = to_cstring(filepath).ok_or_else(|| "Invalid console output path".to_string())?;
        let ret = unsafe { func(ctx_id as c_uint, c_path.as_ptr()) };
        if ret < 0 { Err(format!("krun_set_console_output failed with code {}", ret)) } else { Ok(()) }
    })
}

/// Safe wrapper around `krun_add_vsock`.
#[allow(dead_code)]
pub fn add_vsock(ctx_id: u32, tsi_features: u32) -> Result<(), String> {
    with_lib(|lib| {
        let func: libloading::Symbol<unsafe extern "C" fn(c_uint, c_uint) -> c_int> =
            unsafe { lib.get(b"krun_add_vsock") }.map_err(|e| format!("krun_add_vsock: {}", e))?;
        let ret = unsafe { func(ctx_id as c_uint, tsi_features as c_uint) };
        if ret < 0 { Err(format!("krun_add_vsock failed with code {}", ret)) } else { Ok(()) }
    })
}

/// Safe wrapper around `krun_disable_implicit_vsock`.
#[allow(dead_code)]
pub fn disable_implicit_vsock(ctx_id: u32) -> Result<(), String> {
    with_lib(|lib| {
        let func: libloading::Symbol<unsafe extern "C" fn(c_uint) -> c_int> =
            unsafe { lib.get(b"krun_disable_implicit_vsock") }.map_err(|e| format!("krun_disable_implicit_vsock: {}", e))?;
        let ret = unsafe { func(ctx_id as c_uint) };
        if ret < 0 { Err(format!("krun_disable_implicit_vsock failed with code {}", ret)) } else { Ok(()) }
    })
}

/// Safe wrapper around the deprecated `krun_set_gvproxy_path`.
#[allow(dead_code)]
pub fn set_gvproxy_path(ctx_id: u32, socket_path: &str) -> Result<(), String> {
    with_lib(|lib| {
        let func: libloading::Symbol<unsafe extern "C" fn(c_uint, *const c_char) -> c_int> =
            unsafe { lib.get(b"krun_set_gvproxy_path") }.map_err(|e| format!("krun_set_gvproxy_path: {}", e))?;
        let c_path = to_cstring(socket_path).ok_or_else(|| "Invalid socket path".to_string())?;
        let ret = unsafe { func(ctx_id as c_uint, c_path.as_ptr()) };
        if ret < 0 { Err(format!("krun_set_gvproxy_path failed with code {} (socket: {})", ret, socket_path)) } else { Ok(()) }
    })
}

/// Safe wrapper around `krun_add_net_unixgram`.
#[allow(dead_code)]
pub fn add_net_unixgram(
    ctx_id: u32,
    socket_path: &str,
    mac: &[u8; 6],
    features: u32,
    flags: u32,
) -> Result<(), String> {
    with_lib(|lib| {
        let func: libloading::Symbol<unsafe extern "C" fn(c_uint, *const c_char, c_int, *mut u8, c_uint, c_uint) -> c_int> =
            unsafe { lib.get(b"krun_add_net_unixgram") }.map_err(|e| format!("krun_add_net_unixgram: {}", e))?;
        let c_path = to_cstring(socket_path).ok_or_else(|| "Invalid socket path".to_string())?;
        let mut mac_buf: Vec<u8> = mac.to_vec();
        let ret = unsafe {
            func(ctx_id as c_uint, c_path.as_ptr(), -1 as c_int, mac_buf.as_mut_ptr(), features as c_uint, flags as c_uint)
        };
        if ret < 0 { Err(format!("krun_add_net_unixgram failed with code {} (socket: {})", ret, socket_path)) } else { Ok(()) }
    })
}

/// Safe wrapper around `krun_start_enter`.
/// WARNING: This function does NOT return on success. The calling process is
/// taken over by the microVM. It must be called from a forked child process.
pub fn start_enter(ctx_id: u32) -> Result<(), String> {
    with_lib(|lib| {
        let func: libloading::Symbol<unsafe extern "C" fn(c_uint) -> c_int> =
            unsafe { lib.get(b"krun_start_enter") }.map_err(|e| format!("krun_start_enter: {}", e))?;
        let ret = unsafe { func(ctx_id as c_uint) };
        // If we get here, it failed (success never returns)
        Err(format!("krun_start_enter failed with code {}", ret))
    })
}
