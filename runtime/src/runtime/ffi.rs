//! FFI bindings for libkrun
//!
//! Direct Rust FFI declarations for the libkrun C API.
//! These bindings enable creating and managing microVMs directly
//! via the C API.
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

// ---------------------------------------------------------------------------
// Raw FFI declarations – linked against libkrun at runtime
// macOS: libkrun.dylib (e.g., /opt/homebrew/lib/)
// Linux: libkrun.so (e.g., /usr/lib/, /usr/lib64/, /usr/local/lib/)
// ---------------------------------------------------------------------------
#[link(name = "krun")]
#[allow(dead_code)]
extern "C" {
    /// Set the log level for libkrun.
    /// 0 = Off, 1 = Error, 2 = Warn, 3 = Info, 4 = Debug, 5 = Trace
    pub fn krun_set_log_level(level: c_uint) -> c_int;

    /// Create a new VM context. Returns a context ID (>= 0) on success, or -1 on error.
    pub fn krun_create_ctx() -> c_int;

    /// Free a previously created VM context.
    pub fn krun_free_ctx(ctx_id: c_uint) -> c_int;

    /// Set the VM configuration (vCPUs and memory).
    pub fn krun_set_vm_config(ctx_id: c_uint, num_vcpus: u8, ram_mib: c_uint) -> c_int;

    /// Set the path to the root filesystem for the VM.
    pub fn krun_set_root(ctx_id: c_uint, root_path: *const c_char) -> c_int;

    /// Add a virtiofs mount to the VM.
    pub fn krun_add_virtiofs(
        ctx_id: c_uint,
        tag: *const c_char,
        path: *const c_char,
    ) -> c_int;

    /// Configure a map of host to guest TCP ports.
    /// `port_map` - null-terminated array of "host_port:guest_port" strings.
    /// Pass NULL to expose all listening guest ports to the host.
    pub fn krun_set_port_map(ctx_id: c_uint, port_map: *const *const c_char) -> c_int;

    /// Set the working directory inside the guest.
    pub fn krun_set_workdir(ctx_id: c_uint, workdir: *const c_char) -> c_int;

    /// Set the command to execute inside the VM, with arguments and environment.
    /// `exec_path` - path to the executable
    /// `argv` - null-terminated array of argument strings
    /// `envp` - null-terminated array of "KEY=VALUE" strings, or NULL to inherit host env
    pub fn krun_set_exec(
        ctx_id: c_uint,
        exec_path: *const c_char,
        argv: *const *const c_char,
        envp: *const *const c_char,
    ) -> c_int;

    /// Set environment variables for the VM process.
    /// `envp` - null-terminated array of "KEY=VALUE" strings
    pub fn krun_set_env(ctx_id: c_uint, envp: *const *const c_char) -> c_int;

    /// Configures the implicit console device to write output to a file.
    /// `c_filepath` - path to the file for console output, or NULL
    pub fn krun_set_console_output(ctx_id: c_uint, c_filepath: *const c_char) -> c_int;

    /// Add a vsock device with specified TSI features.
    /// Must call `krun_disable_implicit_vsock` first, then use this to
    /// configure TSI hijack features explicitly.
    ///
    /// `tsi_features` - bitmask:
    ///   KRUN_TSI_HIJACK_INET (1 << 0) = hijack inet sockets
    ///   KRUN_TSI_HIJACK_UNIX (1 << 1) = hijack unix sockets
    ///   0 = vsock without TSI hijacking
    pub fn krun_add_vsock(ctx_id: c_uint, tsi_features: c_uint) -> c_int;

    /// Disable the implicit vsock device. Must be called before `krun_add_vsock`.
    pub fn krun_disable_implicit_vsock(ctx_id: c_uint) -> c_int;

    /// Add a virtio-net device with a unixgram-based backend (e.g., gvproxy).
    ///
    /// When called, TSI networking is automatically disabled and replaced
    /// by the virtio-net device connected to the specified socket.
    ///
    /// `c_path` - path to the unixgram socket (NULL if using `fd`)
    /// `fd` - file descriptor for an open socket (-1 if using `c_path`)
    /// `c_mac` - MAC address as 6-byte array
    /// `features` - virtio-net feature bitmask (e.g., COMPAT_NET_FEATURES)
    /// `flags` - generic flags (e.g., NET_FLAG_VFKIT for gvproxy vfkit mode)
    pub fn krun_add_net_unixgram(
        ctx_id: c_uint,
        c_path: *const c_char,
        fd: c_int,
        c_mac: *mut u8,
        features: c_uint,
        flags: c_uint,
    ) -> c_int;

    /// DEPRECATED. Use krun_add_net_unixgram instead.
    /// Configures networking to use gvproxy in vfkit mode.
    pub fn krun_set_gvproxy_path(ctx_id: c_uint, c_path: *const c_char) -> c_int;

    /// Start the VM and enter it. This function does NOT return on success –
    /// the calling process is taken over by the VM. On error it returns -1.
    ///
    /// Because this never returns, it must be called from a forked child process.
    pub fn krun_start_enter(ctx_id: c_uint) -> c_int;
}

// TSI feature flags (from libkrun.h)
#[allow(dead_code)]
pub const KRUN_TSI_HIJACK_INET: u32 = 1 << 0;
#[allow(dead_code)]
pub const KRUN_TSI_HIJACK_UNIX: u32 = 1 << 1;

// ---------------------------------------------------------------------------
// virtio-net / gvproxy constants (from libkrun.h / krunkit)
// ---------------------------------------------------------------------------

/// Send the VFKIT magic after establishing the connection,
/// as required by gvproxy in vfkit mode.
pub const NET_FLAG_VFKIT: u32 = 1 << 0;

/// virtio-net feature flags for offloading (matching krunkit's COMPAT_NET_FEATURES).
/// These are the features enabled by the legacy krun_set_passt_fd / krun_set_gvproxy_path.
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

/// Default guest MAC address (matches gvproxy's default DHCP static lease).
/// gvproxy assigns 192.168.127.2 to this MAC via DHCP.
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
/// which aborts the process if a global logger is already registered (the panic
/// crosses an `extern "C"` boundary and cannot be caught).
///
/// We probe with `env_logger::try_init_from_env()` first:
/// - If it succeeds, no logger was set yet; we set env_logger ourselves.
///   We then skip `krun_set_log_level` because env_logger is already installed
///   (calling the FFI would attempt to install it a second time and abort).
/// - If it fails, a logger is already present from the host application.
///   We skip `krun_set_log_level` to avoid the abort.
///
/// In both cases libkrun continues to function; the only difference is that
/// its internal log output is controlled by the already-registered logger
/// rather than by `krun_set_log_level`.
pub fn set_log_level(level: u32) -> Result<(), String> {
    // Map libkrun level (0=Off..4=Trace) → env_logger filter
    let env_filter = match level {
        0 => "off",
        1 => "error",
        2 => "warn",
        3 => "info",
        _ => "debug",
    };

    // Try to install env_logger ourselves. If this succeeds, the logger is
    // now registered and we must NOT call krun_set_log_level (which would
    // try env_logger::Builder::init() again and abort).
    let default_env = env_logger::Env::default().default_filter_or(env_filter);
    if env_logger::try_init_from_env(default_env).is_ok() {
        // We just installed env_logger with the desired level — done.
        return Ok(());
    }

    // A logger is already registered by the host application. Skip the FFI
    // call to avoid the abort. libkrun still works.
    Ok(())
}

/// Safe wrapper around `krun_create_ctx`. Returns the context ID.
pub fn create_ctx() -> Result<u32, String> {
    let ret = unsafe { krun_create_ctx() };
    if ret < 0 {
        Err(format!("krun_create_ctx failed with code {}", ret))
    } else {
        Ok(ret as u32)
    }
}

/// Safe wrapper around `krun_free_ctx`.
#[allow(dead_code)]
pub fn free_ctx(ctx_id: u32) -> Result<(), String> {
    let ret = unsafe { krun_free_ctx(ctx_id as c_uint) };
    if ret < 0 {
        Err(format!("krun_free_ctx failed with code {}", ret))
    } else {
        Ok(())
    }
}

/// Safe wrapper around `krun_set_vm_config`.
pub fn set_vm_config(ctx_id: u32, num_vcpus: u8, ram_mib: u32) -> Result<(), String> {
    let ret = unsafe { krun_set_vm_config(ctx_id as c_uint, num_vcpus, ram_mib as c_uint) };
    if ret < 0 {
        Err(format!("krun_set_vm_config failed with code {}", ret))
    } else {
        Ok(())
    }
}

/// Safe wrapper around `krun_set_root`.
pub fn set_root(ctx_id: u32, root_path: &str) -> Result<(), String> {
    let c_path = to_cstring(root_path).ok_or("Invalid root path")?;
    let ret = unsafe { krun_set_root(ctx_id as c_uint, c_path.as_ptr()) };
    if ret < 0 {
        Err(format!("krun_set_root failed with code {}", ret))
    } else {
        Ok(())
    }
}

/// Safe wrapper around `krun_add_virtiofs`.
pub fn add_virtiofs(ctx_id: u32, tag: &str, path: &str) -> Result<(), String> {
    let c_tag = to_cstring(tag).ok_or("Invalid virtiofs tag")?;
    let c_path = to_cstring(path).ok_or("Invalid virtiofs path")?;
    let ret = unsafe { krun_add_virtiofs(ctx_id as c_uint, c_tag.as_ptr(), c_path.as_ptr()) };
    if ret < 0 {
        Err(format!("krun_add_virtiofs failed with code {}", ret))
    } else {
        Ok(())
    }
}

/// Safe wrapper around `krun_set_port_map`.
/// Takes a list of "host_port:guest_port" strings.
/// Pass an empty slice to explicitly disable port forwarding.
/// Pass None to expose all listening guest ports.
pub fn set_port_map(ctx_id: u32, mappings: Option<&[String]>) -> Result<(), String> {
    match mappings {
        None => {
            // NULL = expose all listening guest ports
            let ret = unsafe { krun_set_port_map(ctx_id as c_uint, std::ptr::null()) };
            if ret < 0 {
                Err(format!("krun_set_port_map(NULL) failed with code {}", ret))
            } else {
                Ok(())
            }
        }
        Some(maps) => {
            let (_c_strings, ptrs) = owned_strings_to_c_array(maps);
            let ret = unsafe { krun_set_port_map(ctx_id as c_uint, ptrs.as_ptr()) };
            if ret < 0 {
                Err(format!("krun_set_port_map failed with code {}", ret))
            } else {
                Ok(())
            }
        }
    }
}

/// Safe wrapper around `krun_set_workdir`.
pub fn set_workdir(ctx_id: u32, workdir: &str) -> Result<(), String> {
    let c_workdir = to_cstring(workdir).ok_or("Invalid workdir")?;
    let ret = unsafe { krun_set_workdir(ctx_id as c_uint, c_workdir.as_ptr()) };
    if ret < 0 {
        Err(format!("krun_set_workdir failed with code {}", ret))
    } else {
        Ok(())
    }
}

/// Safe wrapper around `krun_set_exec`.
/// Sets the executable, argv, and environment for the guest process.
/// If `envp` is None, the host's environment is inherited.
pub fn set_exec(ctx_id: u32, exec_path: &str, argv: &[&str], envp: Option<&[String]>) -> Result<(), String> {
    let c_exec = to_cstring(exec_path).ok_or("Invalid exec path")?;
    let (_c_argv, argv_ptrs) = strings_to_c_array(argv);

    let ret = match envp {
        Some(env_vars) => {
            let (_c_env, env_ptrs) = owned_strings_to_c_array(env_vars);
            unsafe { krun_set_exec(ctx_id as c_uint, c_exec.as_ptr(), argv_ptrs.as_ptr(), env_ptrs.as_ptr()) }
        }
        None => {
            unsafe { krun_set_exec(ctx_id as c_uint, c_exec.as_ptr(), argv_ptrs.as_ptr(), std::ptr::null()) }
        }
    };

    if ret < 0 {
        Err(format!("krun_set_exec failed with code {}", ret))
    } else {
        Ok(())
    }
}

/// Safe wrapper around `krun_set_env`.
#[allow(dead_code)]
pub fn set_env(ctx_id: u32, env_vars: &[String]) -> Result<(), String> {
    let (_c_strings, ptrs) = owned_strings_to_c_array(env_vars);
    let ret = unsafe { krun_set_env(ctx_id as c_uint, ptrs.as_ptr()) };
    if ret < 0 {
        Err(format!("krun_set_env failed with code {}", ret))
    } else {
        Ok(())
    }
}

/// Safe wrapper around `krun_set_console_output`.
/// Redirect console output to a file path.
#[allow(dead_code)]
pub fn set_console_output(ctx_id: u32, filepath: &str) -> Result<(), String> {
    let c_path = to_cstring(filepath).ok_or("Invalid console output path")?;
    let ret = unsafe { krun_set_console_output(ctx_id as c_uint, c_path.as_ptr()) };
    if ret < 0 {
        Err(format!("krun_set_console_output failed with code {}", ret))
    } else {
        Ok(())
    }
}

/// Safe wrapper around `krun_add_vsock` for explicit TSI configuration.
/// Must call `disable_implicit_vsock` first.
///
/// `tsi_features`: bitmask of KRUN_TSI_HIJACK_INET | KRUN_TSI_HIJACK_UNIX
#[allow(dead_code)]
pub fn add_vsock(ctx_id: u32, tsi_features: u32) -> Result<(), String> {
    let ret = unsafe { krun_add_vsock(ctx_id as c_uint, tsi_features as c_uint) };
    if ret < 0 {
        Err(format!("krun_add_vsock failed with code {}", ret))
    } else {
        Ok(())
    }
}

/// Safe wrapper around `krun_disable_implicit_vsock`.
#[allow(dead_code)]
pub fn disable_implicit_vsock(ctx_id: u32) -> Result<(), String> {
    let ret = unsafe { krun_disable_implicit_vsock(ctx_id as c_uint) };
    if ret < 0 {
        Err(format!("krun_disable_implicit_vsock failed with code {}", ret))
    } else {
        Ok(())
    }
}

/// Safe wrapper around the deprecated `krun_set_gvproxy_path`.
///
/// Configures networking to use gvproxy in vfkit mode. When called, TSI
/// networking is automatically disabled and replaced by a virtio-net device
/// connected to the gvproxy unixgram socket.
///
/// Note: Prefer `add_net_unixgram()` which is the non-deprecated API.
/// This function is kept for backwards compatibility.
#[allow(dead_code)]
pub fn set_gvproxy_path(ctx_id: u32, socket_path: &str) -> Result<(), String> {
    let c_path = to_cstring(socket_path).ok_or("Invalid socket path")?;
    let ret = unsafe { krun_set_gvproxy_path(ctx_id as c_uint, c_path.as_ptr()) };
    if ret < 0 {
        Err(format!(
            "krun_set_gvproxy_path failed with code {} (socket: {})",
            ret, socket_path
        ))
    } else {
        Ok(())
    }
}

/// Safe wrapper around `krun_add_net_unixgram`.
///
/// Adds a virtio-net device connected to a unixgram socket (e.g., gvproxy).
/// When this is called, TSI networking is automatically disabled.
pub fn add_net_unixgram(
    ctx_id: u32,
    socket_path: &str,
    mac: &[u8; 6],
    features: u32,
    flags: u32,
) -> Result<(), String> {
    let c_path = to_cstring(socket_path).ok_or("Invalid socket path")?;
    let mut mac_buf: Vec<u8> = mac.to_vec();
    let ret = unsafe {
        krun_add_net_unixgram(
            ctx_id as c_uint,
            c_path.as_ptr(),
            -1 as c_int,
            mac_buf.as_mut_ptr(),
            features as c_uint,
            flags as c_uint,
        )
    };
    if ret < 0 {
        Err(format!(
            "krun_add_net_unixgram failed with code {} (socket: {})",
            ret, socket_path
        ))
    } else {
        Ok(())
    }
}

/// Safe wrapper around `krun_start_enter`.
/// WARNING: This function does NOT return on success. The calling process is
/// taken over by the microVM. It must be called from a forked child process.
pub fn start_enter(ctx_id: u32) -> Result<(), String> {
    let ret = unsafe { krun_start_enter(ctx_id as c_uint) };
    // If we get here, it failed (success never returns)
    Err(format!("krun_start_enter failed with code {}", ret))
}
