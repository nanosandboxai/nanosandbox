//! Rust FFI bindings for the libkrun C API.
//!
//! This crate provides both raw `extern "C"` declarations and safe Rust
//! wrappers for the libkrun virtualization library.
//!
//! Reference: <https://github.com/containers/libkrun>
//! Header:    <https://github.com/containers/libkrun/blob/main/include/libkrun.h>

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_uint};

// ---------------------------------------------------------------------------
// Constants (from libkrun.h)
// ---------------------------------------------------------------------------

// TSI feature flags
pub const KRUN_TSI_HIJACK_INET: u32 = 1 << 0;
pub const KRUN_TSI_HIJACK_UNIX: u32 = 1 << 1;

// Logging
pub const KRUN_LOG_TARGET_DEFAULT: i32 = -1;
pub const KRUN_LOG_LEVEL_OFF: u32 = 0;
pub const KRUN_LOG_LEVEL_ERROR: u32 = 1;
pub const KRUN_LOG_LEVEL_WARN: u32 = 2;
pub const KRUN_LOG_LEVEL_INFO: u32 = 3;
pub const KRUN_LOG_LEVEL_DEBUG: u32 = 4;
pub const KRUN_LOG_LEVEL_TRACE: u32 = 5;
pub const KRUN_LOG_STYLE_AUTO: u32 = 0;
pub const KRUN_LOG_STYLE_ALWAYS: u32 = 1;
pub const KRUN_LOG_STYLE_NEVER: u32 = 2;
pub const KRUN_LOG_OPTION_NO_ENV: u32 = 1;

// Disk image formats
pub const KRUN_DISK_FORMAT_RAW: u32 = 0;
pub const KRUN_DISK_FORMAT_QCOW2: u32 = 1;
pub const KRUN_DISK_FORMAT_VMDK: u32 = 2;

// Sync modes for krun_add_disk3
pub const KRUN_SYNC_NONE: u32 = 0;
pub const KRUN_SYNC_RELAXED: u32 = 1;
pub const KRUN_SYNC_FULL: u32 = 2;

// Kernel formats
pub const KRUN_KERNEL_FORMAT_RAW: u32 = 0;
pub const KRUN_KERNEL_FORMAT_ELF: u32 = 1;
pub const KRUN_KERNEL_FORMAT_PE_GZ: u32 = 2;
pub const KRUN_KERNEL_FORMAT_IMAGE_BZ2: u32 = 3;
pub const KRUN_KERNEL_FORMAT_IMAGE_GZ: u32 = 4;
pub const KRUN_KERNEL_FORMAT_IMAGE_ZSTD: u32 = 5;

// Feature constants for krun_has_feature()
pub const KRUN_FEATURE_NET: u64 = 0;
pub const KRUN_FEATURE_BLK: u64 = 1;
pub const KRUN_FEATURE_GPU: u64 = 2;
pub const KRUN_FEATURE_SND: u64 = 3;
pub const KRUN_FEATURE_INPUT: u64 = 4;
pub const KRUN_FEATURE_EFI: u64 = 5;
pub const KRUN_FEATURE_TEE: u64 = 6;
pub const KRUN_FEATURE_AMD_SEV: u64 = 7;
pub const KRUN_FEATURE_INTEL_TDX: u64 = 8;
pub const KRUN_FEATURE_AWS_NITRO: u64 = 9;
pub const KRUN_FEATURE_VIRGL_RESOURCE_MAP2: u64 = 10;

// Display
pub const KRUN_MAX_DISPLAYS: u32 = 16;

// virtio-net / gvproxy
pub const NET_FLAG_VFKIT: u32 = 1 << 0;

const NET_FEATURE_CSUM: u32 = 1 << 0;
const NET_FEATURE_GUEST_CSUM: u32 = 1 << 1;
const NET_FEATURE_GUEST_TSO4: u32 = 1 << 7;
const NET_FEATURE_GUEST_TSO6: u32 = 1 << 8;
const NET_FEATURE_GUEST_UFO: u32 = 1 << 10;
const NET_FEATURE_HOST_TSO4: u32 = 1 << 11;
const NET_FEATURE_HOST_TSO6: u32 = 1 << 12;
const NET_FEATURE_HOST_UFO: u32 = 1 << 14;

pub const COMPAT_NET_FEATURES: u32 = NET_FEATURE_CSUM
    | NET_FEATURE_GUEST_CSUM
    | NET_FEATURE_GUEST_TSO4
    | NET_FEATURE_GUEST_UFO
    | NET_FEATURE_HOST_TSO4
    | NET_FEATURE_HOST_UFO;

pub const FULL_NET_FEATURES: u32 = NET_FEATURE_CSUM
    | NET_FEATURE_GUEST_CSUM
    | NET_FEATURE_GUEST_TSO4
    | NET_FEATURE_GUEST_TSO6
    | NET_FEATURE_GUEST_UFO
    | NET_FEATURE_HOST_TSO4
    | NET_FEATURE_HOST_TSO6
    | NET_FEATURE_HOST_UFO;

/// Default guest MAC address (matches gvproxy's default DHCP static lease).
pub const GVPROXY_GUEST_MAC: [u8; 6] = [0x5a, 0x94, 0xef, 0xe4, 0x0c, 0xee];

// virglrenderer flags
pub const VIRGLRENDERER_USE_EGL: u32 = 1 << 0;
pub const VIRGLRENDERER_THREAD_SYNC: u32 = 1 << 1;
pub const VIRGLRENDERER_USE_GLX: u32 = 1 << 2;
pub const VIRGLRENDERER_USE_SURFACELESS: u32 = 1 << 3;
pub const VIRGLRENDERER_USE_GLES: u32 = 1 << 4;
pub const VIRGLRENDERER_USE_EXTERNAL_BLOB: u32 = 1 << 5;
pub const VIRGLRENDERER_VENUS: u32 = 1 << 6;
pub const VIRGLRENDERER_NO_VIRGL: u32 = 1 << 7;
pub const VIRGLRENDERER_USE_ASYNC_FENCE_CB: u32 = 1 << 8;
pub const VIRGLRENDERER_RENDER_SERVER: u32 = 1 << 9;
pub const VIRGLRENDERER_DRM: u32 = 1 << 10;

// ---------------------------------------------------------------------------
// Raw extern "C" declarations
// ---------------------------------------------------------------------------

extern "C" {
    // --- Logging ---
    pub fn krun_set_log_level(level: c_uint) -> c_int;
    pub fn krun_init_log(target_fd: c_int, level: c_uint, style: c_uint, options: c_uint) -> c_int;

    // --- Context management ---
    pub fn krun_create_ctx() -> c_int;
    pub fn krun_free_ctx(ctx_id: c_uint) -> c_int;

    // --- VM configuration ---
    pub fn krun_set_vm_config(ctx_id: c_uint, num_vcpus: u8, ram_mib: c_uint) -> c_int;
    pub fn krun_set_root(ctx_id: c_uint, root_path: *const c_char) -> c_int;

    // --- Disk management (requires blk feature in libkrun) ---
    #[cfg(feature = "blk")]
    pub fn krun_add_disk(
        ctx_id: c_uint,
        block_id: *const c_char,
        disk_path: *const c_char,
        read_only: bool,
    ) -> c_int;
    #[cfg(feature = "blk")]
    pub fn krun_add_disk2(
        ctx_id: c_uint,
        block_id: *const c_char,
        disk_path: *const c_char,
        disk_format: c_uint,
        read_only: bool,
    ) -> c_int;
    #[cfg(feature = "blk")]
    pub fn krun_add_disk3(
        ctx_id: c_uint,
        block_id: *const c_char,
        disk_path: *const c_char,
        disk_format: c_uint,
        read_only: bool,
        direct_io: bool,
        sync_mode: c_uint,
    ) -> c_int;
    pub fn krun_set_root_disk_remount(
        ctx_id: c_uint,
        device: *const c_char,
        fstype: *const c_char,
        options: *const c_char,
    ) -> c_int;

    // --- Filesystem ---
    pub fn krun_add_virtiofs(ctx_id: c_uint, tag: *const c_char, path: *const c_char) -> c_int;
    pub fn krun_add_virtiofs2(
        ctx_id: c_uint,
        tag: *const c_char,
        path: *const c_char,
        shm_size: u64,
    ) -> c_int;

    // --- Networking ---
    pub fn krun_add_net_unixstream(
        ctx_id: c_uint,
        c_path: *const c_char,
        fd: c_int,
        c_mac: *mut u8,
        features: c_uint,
        flags: c_uint,
    ) -> c_int;
    pub fn krun_add_net_unixgram(
        ctx_id: c_uint,
        c_path: *const c_char,
        fd: c_int,
        c_mac: *mut u8,
        features: c_uint,
        flags: c_uint,
    ) -> c_int;
    pub fn krun_add_net_tap(
        ctx_id: c_uint,
        c_tap_name: *mut c_char,
        c_mac: *mut u8,
        features: c_uint,
        flags: c_uint,
    ) -> c_int;
    pub fn krun_set_net_mac(ctx_id: c_uint, c_mac: *mut u8) -> c_int;
    pub fn krun_set_port_map(ctx_id: c_uint, port_map: *const *const c_char) -> c_int;

    // --- Deprecated networking (kept for backwards compatibility) ---
    #[deprecated(note = "Use krun_add_net_unixgram instead")]
    pub fn krun_set_gvproxy_path(ctx_id: c_uint, path: *const c_char) -> c_int;
    #[deprecated(note = "Use krun_add_net_unixstream instead")]
    pub fn krun_set_passt_fd(ctx_id: c_uint, fd: c_int) -> c_int;

    // --- vsock ---
    pub fn krun_add_vsock(ctx_id: c_uint, tsi_features: c_uint) -> c_int;
    pub fn krun_disable_implicit_vsock(ctx_id: c_uint) -> c_int;
    pub fn krun_add_vsock_port(ctx_id: c_uint, port: c_uint, c_filepath: *const c_char) -> c_int;
    pub fn krun_add_vsock_port2(
        ctx_id: c_uint,
        port: c_uint,
        c_filepath: *const c_char,
        listen: bool,
    ) -> c_int;

    // --- Process execution ---
    pub fn krun_set_workdir(ctx_id: c_uint, workdir: *const c_char) -> c_int;
    pub fn krun_set_exec(
        ctx_id: c_uint,
        exec_path: *const c_char,
        argv: *const *const c_char,
        envp: *const *const c_char,
    ) -> c_int;
    pub fn krun_set_env(ctx_id: c_uint, env: *const *const c_char) -> c_int;
    pub fn krun_set_rlimits(ctx_id: c_uint, rlimits: *const *const c_char) -> c_int;

    // --- Console ---
    pub fn krun_set_console_output(ctx_id: c_uint, filepath: *const c_char) -> c_int;
    pub fn krun_disable_implicit_console(ctx_id: c_uint) -> c_int;
    pub fn krun_set_kernel_console(ctx_id: c_uint, console_id: *const c_char) -> c_int;
    pub fn krun_add_virtio_console_default(
        ctx_id: c_uint,
        input_fd: c_int,
        output_fd: c_int,
        err_fd: c_int,
    ) -> c_int;
    pub fn krun_add_serial_console_default(
        ctx_id: c_uint,
        input_fd: c_int,
        output_fd: c_int,
    ) -> c_int;
    pub fn krun_add_virtio_console_multiport(ctx_id: c_uint) -> c_int;
    pub fn krun_add_console_port_tty(
        ctx_id: c_uint,
        console_id: c_uint,
        name: *const c_char,
        tty_fd: c_int,
    ) -> c_int;
    pub fn krun_add_console_port_inout(
        ctx_id: c_uint,
        console_id: c_uint,
        name: *const c_char,
        input_fd: c_int,
        output_fd: c_int,
    ) -> c_int;

    // --- GPU / Display ---
    pub fn krun_set_gpu_options(ctx_id: c_uint, virgl_flags: c_uint) -> c_int;
    pub fn krun_set_gpu_options2(ctx_id: c_uint, virgl_flags: c_uint, shm_size: u64) -> c_int;
    pub fn krun_add_display(ctx_id: c_uint, width: c_uint, height: c_uint) -> c_int;
    pub fn krun_display_set_edid(
        ctx_id: c_uint,
        display_id: c_uint,
        edid_blob: *const u8,
        blob_size: usize,
    ) -> c_int;
    pub fn krun_display_set_dpi(ctx_id: c_uint, display_id: c_uint, dpi: c_uint) -> c_int;
    pub fn krun_display_set_physical_size(
        ctx_id: c_uint,
        display_id: c_uint,
        width_mm: u16,
        height_mm: u16,
    ) -> c_int;
    pub fn krun_display_set_refresh_rate(
        ctx_id: c_uint,
        display_id: c_uint,
        refresh_rate: c_uint,
    ) -> c_int;
    pub fn krun_set_display_backend(
        ctx_id: c_uint,
        display_backend: *const libc::c_void,
        backend_size: usize,
    ) -> c_int;

    // --- Audio ---
    pub fn krun_set_snd_device(ctx_id: c_uint, enable: bool) -> c_int;

    // --- Input ---
    pub fn krun_add_input_device(
        ctx_id: c_uint,
        config_backend: *const libc::c_void,
        config_backend_size: usize,
        events_backend: *const libc::c_void,
        events_backend_size: usize,
    ) -> c_int;
    pub fn krun_add_input_device_fd(ctx_id: c_uint, input_fd: c_int) -> c_int;

    // --- Kernel / Firmware ---
    pub fn krun_set_firmware(ctx_id: c_uint, firmware_path: *const c_char) -> c_int;
    pub fn krun_set_kernel(
        ctx_id: c_uint,
        kernel_path: *const c_char,
        kernel_format: c_uint,
        initramfs: *const c_char,
        cmdline: *const c_char,
    ) -> c_int;

    // --- Security / TEE ---
    pub fn krun_set_tee_config_file(ctx_id: c_uint, filepath: *const c_char) -> c_int;

    // --- UID/GID (Unix only) ---
    #[cfg(unix)]
    pub fn krun_setuid(ctx_id: c_uint, uid: libc::uid_t) -> c_int;
    #[cfg(unix)]
    pub fn krun_setgid(ctx_id: c_uint, gid: libc::gid_t) -> c_int;

    // --- Nested virtualization ---
    pub fn krun_set_nested_virt(ctx_id: c_uint, enabled: bool) -> c_int;
    pub fn krun_check_nested_virt() -> c_int;

    // --- Feature detection ---
    pub fn krun_has_feature(feature: u64) -> c_int;
    #[cfg(unix)]
    pub fn krun_get_max_vcpus() -> c_int;

    // --- SMBIOS ---
    pub fn krun_set_smbios_oem_strings(ctx_id: c_uint, oem_strings: *const *const c_char) -> c_int;

    // --- IRQ ---
    pub fn krun_split_irqchip(ctx_id: c_uint, enable: bool) -> c_int;

    // --- Shutdown ---
    pub fn krun_get_shutdown_eventfd(ctx_id: c_uint) -> c_int;

    // --- Start ---
    pub fn krun_start_enter(ctx_id: c_uint) -> c_int;
}

// ---------------------------------------------------------------------------
// Helper utilities
// ---------------------------------------------------------------------------

fn to_cstring(s: &str) -> Option<CString> {
    CString::new(s).ok()
}

fn strings_to_c_array(strings: &[&str]) -> (Vec<CString>, Vec<*const c_char>) {
    let c_strings: Vec<CString> = strings.iter().filter_map(|s| to_cstring(s)).collect();
    let mut ptrs: Vec<*const c_char> = c_strings.iter().map(|cs| cs.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    (c_strings, ptrs)
}

fn owned_strings_to_c_array(strings: &[String]) -> (Vec<CString>, Vec<*const c_char>) {
    let c_strings: Vec<CString> = strings.iter().filter_map(|s| to_cstring(s)).collect();
    let mut ptrs: Vec<*const c_char> = c_strings.iter().map(|cs| cs.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    (c_strings, ptrs)
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers — Core
// ---------------------------------------------------------------------------

pub fn safe_create_ctx() -> Result<u32, String> {
    let ret = unsafe { krun_create_ctx() };
    if ret < 0 {
        Err(format!("krun_create_ctx failed with code {}", ret))
    } else {
        Ok(ret as u32)
    }
}

pub fn safe_free_ctx(ctx_id: u32) -> Result<(), String> {
    let ret = unsafe { krun_free_ctx(ctx_id as c_uint) };
    if ret < 0 {
        Err(format!("krun_free_ctx failed with code {}", ret))
    } else {
        Ok(())
    }
}

pub fn safe_set_vm_config(ctx_id: u32, num_vcpus: u8, ram_mib: u32) -> Result<(), String> {
    let ret = unsafe { krun_set_vm_config(ctx_id as c_uint, num_vcpus, ram_mib as c_uint) };
    if ret < 0 {
        Err(format!("krun_set_vm_config failed with code {}", ret))
    } else {
        Ok(())
    }
}

pub fn safe_set_root(ctx_id: u32, root_path: &str) -> Result<(), String> {
    let c_path = to_cstring(root_path).ok_or_else(|| "Invalid root path".to_string())?;
    let ret = unsafe { krun_set_root(ctx_id as c_uint, c_path.as_ptr()) };
    if ret < 0 {
        Err(format!("krun_set_root failed with code {}", ret))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers — Disk management
// ---------------------------------------------------------------------------

#[cfg(feature = "blk")]
pub fn safe_add_disk(
    ctx_id: u32,
    block_id: &str,
    disk_path: &str,
    read_only: bool,
) -> Result<(), String> {
    let c_block = to_cstring(block_id).ok_or_else(|| "Invalid block_id".to_string())?;
    let c_path = to_cstring(disk_path).ok_or_else(|| "Invalid disk_path".to_string())?;
    let ret = unsafe {
        krun_add_disk(
            ctx_id as c_uint,
            c_block.as_ptr(),
            c_path.as_ptr(),
            read_only,
        )
    };
    if ret < 0 {
        Err(format!("krun_add_disk failed with code {}", ret))
    } else {
        Ok(())
    }
}

#[cfg(feature = "blk")]
pub fn safe_add_disk2(
    ctx_id: u32,
    block_id: &str,
    disk_path: &str,
    disk_format: u32,
    read_only: bool,
) -> Result<(), String> {
    let c_block = to_cstring(block_id).ok_or_else(|| "Invalid block_id".to_string())?;
    let c_path = to_cstring(disk_path).ok_or_else(|| "Invalid disk_path".to_string())?;
    let ret = unsafe {
        krun_add_disk2(
            ctx_id as c_uint,
            c_block.as_ptr(),
            c_path.as_ptr(),
            disk_format as c_uint,
            read_only,
        )
    };
    if ret < 0 {
        Err(format!("krun_add_disk2 failed with code {}", ret))
    } else {
        Ok(())
    }
}

#[cfg(feature = "blk")]
pub fn safe_add_disk3(
    ctx_id: u32,
    block_id: &str,
    disk_path: &str,
    disk_format: u32,
    read_only: bool,
    direct_io: bool,
    sync_mode: u32,
) -> Result<(), String> {
    let c_block = to_cstring(block_id).ok_or_else(|| "Invalid block_id".to_string())?;
    let c_path = to_cstring(disk_path).ok_or_else(|| "Invalid disk_path".to_string())?;
    let ret = unsafe {
        krun_add_disk3(
            ctx_id as c_uint,
            c_block.as_ptr(),
            c_path.as_ptr(),
            disk_format as c_uint,
            read_only,
            direct_io,
            sync_mode as c_uint,
        )
    };
    if ret < 0 {
        Err(format!("krun_add_disk3 failed with code {}", ret))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers — Filesystem
// ---------------------------------------------------------------------------

pub fn safe_add_virtiofs(ctx_id: u32, tag: &str, path: &str) -> Result<(), String> {
    let c_tag = to_cstring(tag).ok_or_else(|| "Invalid virtiofs tag".to_string())?;
    let c_path = to_cstring(path).ok_or_else(|| "Invalid virtiofs path".to_string())?;
    let ret = unsafe { krun_add_virtiofs(ctx_id as c_uint, c_tag.as_ptr(), c_path.as_ptr()) };
    if ret < 0 {
        Err(format!("krun_add_virtiofs failed with code {}", ret))
    } else {
        Ok(())
    }
}

pub fn safe_add_virtiofs2(ctx_id: u32, tag: &str, path: &str, shm_size: u64) -> Result<(), String> {
    let c_tag = to_cstring(tag).ok_or_else(|| "Invalid virtiofs tag".to_string())?;
    let c_path = to_cstring(path).ok_or_else(|| "Invalid virtiofs path".to_string())?;
    let ret =
        unsafe { krun_add_virtiofs2(ctx_id as c_uint, c_tag.as_ptr(), c_path.as_ptr(), shm_size) };
    if ret < 0 {
        Err(format!("krun_add_virtiofs2 failed with code {}", ret))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers — Networking
// ---------------------------------------------------------------------------

pub fn safe_set_port_map(ctx_id: u32, mappings: Option<&[String]>) -> Result<(), String> {
    match mappings {
        None => {
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

pub fn safe_add_net_unixstream(
    ctx_id: u32,
    socket_path: &str,
    mac: &[u8; 6],
    features: u32,
    flags: u32,
) -> Result<(), String> {
    let c_path = to_cstring(socket_path).ok_or_else(|| "Invalid socket path".to_string())?;
    let mut mac_buf: Vec<u8> = mac.to_vec();
    let ret = unsafe {
        krun_add_net_unixstream(
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
            "krun_add_net_unixstream failed with code {} (socket: {})",
            ret, socket_path
        ))
    } else {
        Ok(())
    }
}

pub fn safe_add_net_unixgram(
    ctx_id: u32,
    socket_path: &str,
    mac: &[u8; 6],
    features: u32,
    flags: u32,
) -> Result<(), String> {
    let c_path = to_cstring(socket_path).ok_or_else(|| "Invalid socket path".to_string())?;
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

/// Deprecated: use safe_add_net_unixgram instead.
#[deprecated(note = "Use safe_add_net_unixgram instead")]
#[allow(deprecated)]
pub fn safe_set_gvproxy_path(ctx_id: u32, socket_path: &str) -> Result<(), String> {
    let c_path = to_cstring(socket_path).ok_or_else(|| "Invalid socket path".to_string())?;
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

// ---------------------------------------------------------------------------
// Safe Rust wrappers — vsock
// ---------------------------------------------------------------------------

pub fn safe_add_vsock(ctx_id: u32, tsi_features: u32) -> Result<(), String> {
    let ret = unsafe { krun_add_vsock(ctx_id as c_uint, tsi_features as c_uint) };
    if ret < 0 {
        Err(format!("krun_add_vsock failed with code {}", ret))
    } else {
        Ok(())
    }
}

pub fn safe_disable_implicit_vsock(ctx_id: u32) -> Result<(), String> {
    let ret = unsafe { krun_disable_implicit_vsock(ctx_id as c_uint) };
    if ret < 0 {
        Err(format!(
            "krun_disable_implicit_vsock failed with code {}",
            ret
        ))
    } else {
        Ok(())
    }
}

pub fn safe_add_vsock_port(ctx_id: u32, port: u32, filepath: &str) -> Result<(), String> {
    let c_path = to_cstring(filepath).ok_or_else(|| "Invalid vsock port path".to_string())?;
    let ret = unsafe { krun_add_vsock_port(ctx_id as c_uint, port as c_uint, c_path.as_ptr()) };
    if ret < 0 {
        Err(format!("krun_add_vsock_port failed with code {}", ret))
    } else {
        Ok(())
    }
}

pub fn safe_add_vsock_port2(
    ctx_id: u32,
    port: u32,
    filepath: &str,
    listen: bool,
) -> Result<(), String> {
    let c_path = to_cstring(filepath).ok_or_else(|| "Invalid vsock port path".to_string())?;
    let ret =
        unsafe { krun_add_vsock_port2(ctx_id as c_uint, port as c_uint, c_path.as_ptr(), listen) };
    if ret < 0 {
        Err(format!("krun_add_vsock_port2 failed with code {}", ret))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers — Process execution
// ---------------------------------------------------------------------------

pub fn safe_set_workdir(ctx_id: u32, workdir: &str) -> Result<(), String> {
    let c_workdir = to_cstring(workdir).ok_or_else(|| "Invalid workdir".to_string())?;
    let ret = unsafe { krun_set_workdir(ctx_id as c_uint, c_workdir.as_ptr()) };
    if ret < 0 {
        Err(format!("krun_set_workdir failed with code {}", ret))
    } else {
        Ok(())
    }
}

pub fn safe_set_exec(
    ctx_id: u32,
    exec_path: &str,
    argv: &[&str],
    envp: Option<&[String]>,
) -> Result<(), String> {
    let c_exec = to_cstring(exec_path).ok_or_else(|| "Invalid exec path".to_string())?;
    let (_c_argv, argv_ptrs) = strings_to_c_array(argv);
    let ret = match envp {
        Some(env_vars) => {
            let (_c_env, env_ptrs) = owned_strings_to_c_array(env_vars);
            unsafe {
                krun_set_exec(
                    ctx_id as c_uint,
                    c_exec.as_ptr(),
                    argv_ptrs.as_ptr(),
                    env_ptrs.as_ptr(),
                )
            }
        }
        None => unsafe {
            krun_set_exec(
                ctx_id as c_uint,
                c_exec.as_ptr(),
                argv_ptrs.as_ptr(),
                std::ptr::null(),
            )
        },
    };
    if ret < 0 {
        Err(format!("krun_set_exec failed with code {}", ret))
    } else {
        Ok(())
    }
}

pub fn safe_set_env(ctx_id: u32, env_vars: &[String]) -> Result<(), String> {
    let (_c_strings, ptrs) = owned_strings_to_c_array(env_vars);
    let ret = unsafe { krun_set_env(ctx_id as c_uint, ptrs.as_ptr()) };
    if ret < 0 {
        Err(format!("krun_set_env failed with code {}", ret))
    } else {
        Ok(())
    }
}

pub fn safe_set_rlimits(ctx_id: u32, rlimits: &[String]) -> Result<(), String> {
    let (_c_strings, ptrs) = owned_strings_to_c_array(rlimits);
    let ret = unsafe { krun_set_rlimits(ctx_id as c_uint, ptrs.as_ptr()) };
    if ret < 0 {
        Err(format!("krun_set_rlimits failed with code {}", ret))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers — Console
// ---------------------------------------------------------------------------

pub fn safe_set_console_output(ctx_id: u32, filepath: &str) -> Result<(), String> {
    let c_path = to_cstring(filepath).ok_or_else(|| "Invalid console output path".to_string())?;
    let ret = unsafe { krun_set_console_output(ctx_id as c_uint, c_path.as_ptr()) };
    if ret < 0 {
        Err(format!("krun_set_console_output failed with code {}", ret))
    } else {
        Ok(())
    }
}

pub fn safe_disable_implicit_console(ctx_id: u32) -> Result<(), String> {
    let ret = unsafe { krun_disable_implicit_console(ctx_id as c_uint) };
    if ret < 0 {
        Err(format!(
            "krun_disable_implicit_console failed with code {}",
            ret
        ))
    } else {
        Ok(())
    }
}

pub fn safe_add_virtio_console_default(
    ctx_id: u32,
    input_fd: i32,
    output_fd: i32,
    err_fd: i32,
) -> Result<(), String> {
    let ret = unsafe {
        krun_add_virtio_console_default(
            ctx_id as c_uint,
            input_fd as c_int,
            output_fd as c_int,
            err_fd as c_int,
        )
    };
    if ret < 0 {
        Err(format!(
            "krun_add_virtio_console_default failed with code {}",
            ret
        ))
    } else {
        Ok(())
    }
}

pub fn safe_add_serial_console_default(
    ctx_id: u32,
    input_fd: i32,
    output_fd: i32,
) -> Result<(), String> {
    let ret = unsafe {
        krun_add_serial_console_default(ctx_id as c_uint, input_fd as c_int, output_fd as c_int)
    };
    if ret < 0 {
        Err(format!(
            "krun_add_serial_console_default failed with code {}",
            ret
        ))
    } else {
        Ok(())
    }
}

pub fn safe_add_virtio_console_multiport(ctx_id: u32) -> Result<u32, String> {
    let ret = unsafe { krun_add_virtio_console_multiport(ctx_id as c_uint) };
    if ret < 0 {
        Err(format!(
            "krun_add_virtio_console_multiport failed with code {}",
            ret
        ))
    } else {
        Ok(ret as u32)
    }
}

pub fn safe_add_console_port_tty(
    ctx_id: u32,
    console_id: u32,
    name: &str,
    tty_fd: i32,
) -> Result<(), String> {
    let c_name = to_cstring(name).ok_or_else(|| "Invalid port name".to_string())?;
    let ret = unsafe {
        krun_add_console_port_tty(
            ctx_id as c_uint,
            console_id as c_uint,
            c_name.as_ptr(),
            tty_fd as c_int,
        )
    };
    if ret < 0 {
        Err(format!(
            "krun_add_console_port_tty failed with code {}",
            ret
        ))
    } else {
        Ok(())
    }
}

pub fn safe_add_console_port_inout(
    ctx_id: u32,
    console_id: u32,
    name: &str,
    input_fd: i32,
    output_fd: i32,
) -> Result<(), String> {
    let c_name = to_cstring(name).ok_or_else(|| "Invalid port name".to_string())?;
    let ret = unsafe {
        krun_add_console_port_inout(
            ctx_id as c_uint,
            console_id as c_uint,
            c_name.as_ptr(),
            input_fd as c_int,
            output_fd as c_int,
        )
    };
    if ret < 0 {
        Err(format!(
            "krun_add_console_port_inout failed with code {}",
            ret
        ))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers — GPU / Display
// ---------------------------------------------------------------------------

pub fn safe_set_gpu_options(ctx_id: u32, virgl_flags: u32) -> Result<(), String> {
    let ret = unsafe { krun_set_gpu_options(ctx_id as c_uint, virgl_flags as c_uint) };
    if ret < 0 {
        Err(format!("krun_set_gpu_options failed with code {}", ret))
    } else {
        Ok(())
    }
}

pub fn safe_set_gpu_options2(ctx_id: u32, virgl_flags: u32, shm_size: u64) -> Result<(), String> {
    let ret = unsafe { krun_set_gpu_options2(ctx_id as c_uint, virgl_flags as c_uint, shm_size) };
    if ret < 0 {
        Err(format!("krun_set_gpu_options2 failed with code {}", ret))
    } else {
        Ok(())
    }
}

pub fn safe_add_display(ctx_id: u32, width: u32, height: u32) -> Result<u32, String> {
    let ret = unsafe { krun_add_display(ctx_id as c_uint, width as c_uint, height as c_uint) };
    if ret < 0 {
        Err(format!("krun_add_display failed with code {}", ret))
    } else {
        Ok(ret as u32)
    }
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers — Audio / Input
// ---------------------------------------------------------------------------

pub fn safe_set_snd_device(ctx_id: u32, enable: bool) -> Result<(), String> {
    let ret = unsafe { krun_set_snd_device(ctx_id as c_uint, enable) };
    if ret < 0 {
        Err(format!("krun_set_snd_device failed with code {}", ret))
    } else {
        Ok(())
    }
}

pub fn safe_add_input_device_fd(ctx_id: u32, input_fd: i32) -> Result<(), String> {
    let ret = unsafe { krun_add_input_device_fd(ctx_id as c_uint, input_fd as c_int) };
    if ret < 0 {
        Err(format!("krun_add_input_device_fd failed with code {}", ret))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers — Kernel / Firmware
// ---------------------------------------------------------------------------

pub fn safe_set_firmware(ctx_id: u32, firmware_path: &str) -> Result<(), String> {
    let c_path = to_cstring(firmware_path).ok_or_else(|| "Invalid firmware path".to_string())?;
    let ret = unsafe { krun_set_firmware(ctx_id as c_uint, c_path.as_ptr()) };
    if ret < 0 {
        Err(format!("krun_set_firmware failed with code {}", ret))
    } else {
        Ok(())
    }
}

pub fn safe_set_kernel(
    ctx_id: u32,
    kernel_path: &str,
    kernel_format: u32,
    initramfs: Option<&str>,
    cmdline: Option<&str>,
) -> Result<(), String> {
    let c_kernel = to_cstring(kernel_path).ok_or_else(|| "Invalid kernel path".to_string())?;
    let c_initramfs = initramfs.and_then(to_cstring);
    let c_cmdline = cmdline.and_then(to_cstring);
    let ret = unsafe {
        krun_set_kernel(
            ctx_id as c_uint,
            c_kernel.as_ptr(),
            kernel_format as c_uint,
            c_initramfs
                .as_ref()
                .map_or(std::ptr::null(), |c| c.as_ptr()),
            c_cmdline.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
        )
    };
    if ret < 0 {
        Err(format!("krun_set_kernel failed with code {}", ret))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers — UID/GID
// ---------------------------------------------------------------------------

#[cfg(unix)]
pub fn safe_setuid(ctx_id: u32, uid: u32) -> Result<(), String> {
    let ret = unsafe { krun_setuid(ctx_id as c_uint, uid) };
    if ret < 0 {
        Err(format!("krun_setuid failed with code {}", ret))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
pub fn safe_setgid(ctx_id: u32, gid: u32) -> Result<(), String> {
    let ret = unsafe { krun_setgid(ctx_id as c_uint, gid) };
    if ret < 0 {
        Err(format!("krun_setgid failed with code {}", ret))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers — Nested virtualization
// ---------------------------------------------------------------------------

pub fn safe_set_nested_virt(ctx_id: u32, enabled: bool) -> Result<(), String> {
    let ret = unsafe { krun_set_nested_virt(ctx_id as c_uint, enabled) };
    if ret < 0 {
        Err(format!("krun_set_nested_virt failed with code {}", ret))
    } else {
        Ok(())
    }
}

pub fn safe_check_nested_virt() -> Result<bool, String> {
    let ret = unsafe { krun_check_nested_virt() };
    if ret < 0 {
        Err(format!("krun_check_nested_virt failed with code {}", ret))
    } else {
        Ok(ret == 1)
    }
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers — Feature detection
// ---------------------------------------------------------------------------

/// Check if a specific feature was enabled at build time.
/// Returns true if supported, false if not.
pub fn safe_has_feature(feature: u64) -> Result<bool, String> {
    let ret = unsafe { krun_has_feature(feature) };
    if ret < 0 {
        Err(format!("krun_has_feature failed with code {}", ret))
    } else {
        Ok(ret == 1)
    }
}

/// Get the maximum number of vCPUs supported by the hypervisor.
#[cfg(unix)]
pub fn safe_get_max_vcpus() -> Result<u32, String> {
    let ret = unsafe { krun_get_max_vcpus() };
    if ret < 0 {
        Err(format!("krun_get_max_vcpus failed with code {}", ret))
    } else {
        Ok(ret as u32)
    }
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers — SMBIOS
// ---------------------------------------------------------------------------

pub fn safe_set_smbios_oem_strings(ctx_id: u32, strings: &[String]) -> Result<(), String> {
    let (_c_strings, ptrs) = owned_strings_to_c_array(strings);
    let ret = unsafe { krun_set_smbios_oem_strings(ctx_id as c_uint, ptrs.as_ptr()) };
    if ret < 0 {
        Err(format!(
            "krun_set_smbios_oem_strings failed with code {}",
            ret
        ))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers — Logging
// ---------------------------------------------------------------------------

pub fn safe_set_log_level(level: u32) -> Result<(), String> {
    let ret = unsafe { krun_set_log_level(level as c_uint) };
    if ret < 0 {
        Err(format!("krun_set_log_level failed with code {}", ret))
    } else {
        Ok(())
    }
}

pub fn safe_init_log(target_fd: i32, level: u32) -> Result<(), String> {
    let ret = unsafe {
        krun_init_log(
            target_fd as c_int,
            level as c_uint,
            KRUN_LOG_STYLE_NEVER as c_uint,
            KRUN_LOG_OPTION_NO_ENV as c_uint,
        )
    };
    if ret < 0 {
        Err(format!("krun_init_log failed with code {}", ret))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers — Start / Shutdown
// ---------------------------------------------------------------------------

/// WARNING: This function does NOT return on success. The calling process is
/// taken over by the microVM. It must be called from a forked child process.
pub fn safe_start_enter(ctx_id: u32) -> Result<(), String> {
    let ret = unsafe { krun_start_enter(ctx_id as c_uint) };
    Err(format!("krun_start_enter failed with code {}", ret))
}

pub fn safe_get_shutdown_eventfd(ctx_id: u32) -> Result<i32, String> {
    let ret = unsafe { krun_get_shutdown_eventfd(ctx_id as c_uint) };
    if ret < 0 {
        Err(format!(
            "krun_get_shutdown_eventfd failed with code {}",
            ret
        ))
    } else {
        Ok(ret)
    }
}

