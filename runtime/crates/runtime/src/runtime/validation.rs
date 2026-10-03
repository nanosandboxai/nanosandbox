//! Runtime prerequisite validation
//!
//! This module provides validation checks for runtime prerequisites
//! on each supported platform.

use crate::error::{Error, Result};
use tracing::{debug, error, info, warn};

#[cfg(target_os = "linux")]
use std::path::Path;

/// Individual prerequisite check error
#[derive(Debug, Clone)]
pub struct PrerequisiteError {
    /// Name of the check that failed
    pub check: String,
    /// Error message
    pub message: String,
    /// How to fix it (install hint for CLI to display)
    pub fix_hint: Option<String>,
}

impl std::fmt::Display for PrerequisiteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "- {}: {}", self.check, self.message)?;
        if let Some(hint) = &self.fix_hint {
            write!(f, "\n  Fix: {}", hint)?;
        }
        Ok(())
    }
}

/// Validation result containing all check outcomes
#[derive(Debug, Default)]
pub struct ValidationResult {
    /// List of errors encountered
    pub errors: Vec<PrerequisiteError>,
    /// List of warnings (non-fatal)
    pub warnings: Vec<String>,
}

impl ValidationResult {
    /// Check if validation passed (no errors)
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// Convert to Result, failing if there are errors
    pub fn into_result(self) -> Result<()> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            let msg = self
                .errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            error!("Runtime prerequisites not available:\n{}", msg);
            Err(Error::RuntimeNotAvailable(msg))
        }
    }

    /// Add an error
    pub fn add_error(
        &mut self,
        check: impl Into<String>,
        message: impl Into<String>,
        fix_hint: Option<String>,
    ) {
        self.errors.push(PrerequisiteError {
            check: check.into(),
            message: message.into(),
            fix_hint,
        });
    }

    /// Add a warning
    pub fn add_warning(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }
}

/// Validate runtime prerequisites for the current platform
pub async fn validate_runtime_prerequisites() -> Result<()> {
    validate_runtime_prerequisites_detailed()
        .await
        .into_result()
}

/// Validate runtime prerequisites, returning detailed results
///
/// Unlike `validate_runtime_prerequisites()` which returns `Result<()>`,
/// this function returns the raw `ValidationResult` so callers can inspect
/// individual check outcomes (used by the `doctor` command).
pub async fn validate_runtime_prerequisites_detailed() -> ValidationResult {
    #[cfg(target_os = "windows")]
    {
        validate_windows_detailed().await
    }

    #[cfg(target_os = "linux")]
    {
        validate_linux_detailed().await
    }

    #[cfg(target_os = "macos")]
    {
        validate_macos_detailed().await
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let mut result = ValidationResult::default();
        result.add_error(
            "Platform",
            format!("Unsupported platform: {}", std::env::consts::OS),
            None,
        );
        result
    }
}

// ===== Windows Validation =====

#[cfg(target_os = "windows")]
#[allow(dead_code)]
async fn validate_windows_prerequisites() -> Result<()> {
    validate_windows_detailed().await.into_result()
}

#[cfg(target_os = "windows")]
async fn validate_windows_detailed() -> ValidationResult {
    info!("Validating Windows HCS runtime prerequisites...");
    let mut result = ValidationResult::default();

    // Check 1: HCS service running (vmcompute) — core requirement
    debug!("Checking HCS service...");
    let hcs_running = check_hcs_service().await;
    if !hcs_running {
        result.add_error(
            "HCS Service",
            "vmcompute service (Host Compute Service) is not running",
            Some(
                "Ensure Hyper-V is enabled and start the HCS service:\n\
                 Run: Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -All\n\
                 Run: Start-Service vmcompute"
                    .to_string(),
            ),
        );
    }

    // Check 2: Hyper-V access for the current user.
    // The HCS APIs require the caller to be a member of the local
    // "Hyper-V Administrators" group, or to be running elevated as
    // Administrator. Without either, sandbox creation fails with confusing
    // access-denied errors from vmcompute. (issue #133)
    debug!("Checking Hyper-V access for current user...");
    if !check_hyperv_access().await {
        result.add_error(
            "Hyper-V Access",
            "Current user is not in the 'Hyper-V Administrators' group and is not running as Administrator. \
             HCS will reject sandbox creation.",
            Some(
                "Add the current user to the Hyper-V Administrators group:\n\
                 Run in an elevated PowerShell:\n\
                     Add-LocalGroupMember -Group 'Hyper-V Administrators' -Member $env:USERNAME\n\
                 Then log out and back in for the group to take effect.\n\
                 Alternatively, re-run nanosb in an elevated terminal."
                    .to_string(),
            ),
        );
    }

    // Check 3: WSL kernel (HCS uses the WSL2 kernel to boot Linux VMs)
    debug!("Checking WSL kernel...");
    let wsl_kernel = r"C:\Program Files\WSL\tools\kernel";
    if !std::path::Path::new(wsl_kernel).exists() {
        result.add_error(
            "WSL Kernel",
            "WSL kernel not found. HCS needs the WSL2 kernel to boot Linux VMs.",
            Some("Install WSL with: wsl --install --no-distribution".to_string()),
        );
    }

    // Check 4: libkrunfw.dll (kernel firmware, loaded at runtime by libkrun)
    debug!("Checking libkrunfw.dll...");
    let found_krunfw = check_libkrunfw_dll();
    if !found_krunfw {
        result.add_error(
            "libkrunfw.dll",
            "libkrunfw.dll not found. Required for VM boot.",
            Some(
                "Install runtime deps:\n\
                 irm https://github.com/nanosandboxai/nanosandbox/releases/latest/download/install-deps.ps1 | iex"
                    .to_string(),
            ),
        );
    }

    // Check 5: userspace helper binaries required by initrd on Windows
    for dep in ["busybox", "vsock_proxy", "fuse_mount"] {
        debug!("Checking {}...", dep);
        if !check_windows_runtime_dep(dep) {
            result.add_error(
                dep,
                format!("{} not found. Required for Windows VM boot path.", dep),
                Some(
                    "Install runtime deps:\n\
                     irm https://github.com/nanosandboxai/nanosandbox/releases/latest/download/install-deps.ps1 | iex"
                        .to_string(),
                ),
            );
        }
    }

    // Check 6: Disk performance (SSD recommended for fast rootfs / boot)
    debug!("Checking disk performance characteristics...");
    check_windows_disk_performance(&mut result).await;

    // Check 7: System resources (RAM)
    debug!("Checking system resources...");
    check_windows_resources(&mut result);

    // Check 8: Disk space (rootfs + cached images need several GB)
    debug!("Checking disk space...");
    check_windows_disk_space(&mut result);

    // Log warnings
    for warning in &result.warnings {
        warn!("{}", warning);
    }

    result
}

#[cfg(target_os = "windows")]
async fn check_windows_disk_performance(result: &mut ValidationResult) {
    use tokio::process::Command;

    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "(Get-PhysicalDisk | Where-Object { $_.DeviceId -eq 0 }).MediaType",
        ])
        .output()
        .await;

    if let Ok(out) = output {
        let media_type = String::from_utf8_lossy(&out.stdout).trim().to_string();
        debug!("System disk media type: {:?}", media_type);
        if media_type.eq_ignore_ascii_case("HDD") || media_type.eq_ignore_ascii_case("Unspecified") {
            result.add_warning(
                "No SSD detected. The first microVM boot may be noticeably slower on a hard drive. \
                 An SSD is recommended for the best experience. Subsequent boots use a cache and are fast."
                    .to_string(),
            );
        }
    }
}

#[cfg(target_os = "windows")]
fn check_windows_resources(result: &mut ValidationResult) {
    #[repr(C)]
    #[allow(non_snake_case)]
    struct MEMORYSTATUSEX {
        dwLength: u32,
        dwMemoryLoad: u32,
        ullTotalPhys: u64,
        ullAvailPhys: u64,
        ullTotalPageFile: u64,
        ullAvailPageFile: u64,
        ullTotalVirtual: u64,
        ullAvailVirtual: u64,
        ullAvailExtendedVirtual: u64,
    }

    extern "system" {
        fn GlobalMemoryStatusEx(lpBuffer: *mut MEMORYSTATUSEX) -> i32;
    }

    let mut mem = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        dwMemoryLoad: 0,
        ullTotalPhys: 0,
        ullAvailPhys: 0,
        ullTotalPageFile: 0,
        ullAvailPageFile: 0,
        ullTotalVirtual: 0,
        ullAvailVirtual: 0,
        ullAvailExtendedVirtual: 0,
    };

    let ok = unsafe { GlobalMemoryStatusEx(&mut mem) };
    if ok != 0 {
        let avail_gb = mem.ullAvailPhys as f64 / (1024.0 * 1024.0 * 1024.0);
        debug!("Available RAM: {:.1}GB", avail_gb);

        if avail_gb < 4.0 {
            result.add_warning(
                "Low available memory. The first microVM boot needs a few GB of free RAM \
                 to prepare the disk image efficiently. Performance may be degraded."
                    .to_string(),
            );
        }
    }
}

#[cfg(target_os = "windows")]
fn check_windows_disk_space(result: &mut ValidationResult) {
    #[allow(non_snake_case)]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            lpDirectoryName: *const u16,
            lpFreeBytesAvailableToCaller: *mut u64,
            lpTotalNumberOfBytes: *mut u64,
            lpTotalNumberOfFreeBytes: *mut u64,
        ) -> i32;
    }

    let nanosandbox_dir = dirs::home_dir()
        .map(|h| h.join(".nanosandbox"))
        .unwrap_or_default();

    // Use .nanosandbox dir if it exists, otherwise use the home drive
    let check_path = if nanosandbox_dir.exists() {
        nanosandbox_dir
    } else {
        dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("C:\\"))
    };

    let wide_path: Vec<u16> = check_path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let mut free_bytes: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut total_free: u64 = 0;

    let ok = unsafe {
        GetDiskFreeSpaceExW(wide_path.as_ptr(), &mut free_bytes, &mut total_bytes, &mut total_free)
    };

    if ok != 0 {
        let free_gb = free_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        debug!("Disk space available: {:.1} GB at {}", free_gb, check_path.display());

        if free_gb < 2.0 {
            result.add_error(
                "Disk Space",
                &format!(
                    "Only {:.1} GB free on {}. At least 2 GB is required for VM disk images.",
                    free_gb,
                    check_path.display()
                ),
                Some("Free up disk space or run: nanosb prune".to_string()),
            );
        } else if free_gb < 5.0 {
            result.add_warning(format!(
                "Low disk space ({:.1} GB free). VM images and caches may need several GB. \
                 Consider freeing space or running: nanosb prune",
                free_gb
            ));
        }
    }
}

#[cfg(target_os = "windows")]
fn check_libkrunfw_dll() -> bool {
    check_windows_runtime_dep("libkrunfw.dll")
}

#[cfg(target_os = "windows")]
fn check_windows_runtime_dep(name: &str) -> bool {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();

    // ~/.nanosandbox/libs/ (current layout)
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".nanosandbox").join("libs").join(name));
        // Legacy: old install-deps placed files at root level
        candidates.push(home.join(".nanosandbox").join(name));
    }

    // Next to the current executable (custom install dir)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("libs").join(name));
            candidates.push(dir.join(name));
        }
    }

    candidates.iter().any(|p| {
        if p.exists() {
            debug!("Found {} at: {}", name, p.display());
            true
        } else {
            false
        }
    })
}

#[cfg(target_os = "windows")]
async fn check_hcs_service() -> bool {
    use tokio::process::Command;

    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "(Get-Service vmcompute -ErrorAction SilentlyContinue).Status -eq 'Running'",
        ])
        .output()
        .await;

    if let Ok(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        return stdout.trim().eq_ignore_ascii_case("true");
    }
    false
}

/// Check whether the current Windows user can talk to HCS.
///
/// HCS requires the caller to be a member of the local "Hyper-V
/// Administrators" group (well-known SID `S-1-5-32-578`) or to be running
/// elevated as Administrator (well-known SID `S-1-5-32-544`). Anything else
/// gets a confusing access-denied error from vmcompute, so doctor surfaces
/// this up front.
#[cfg(target_os = "windows")]
async fn check_hyperv_access() -> bool {
    use tokio::process::Command;

    let script = "\
        $identity = [Security.Principal.WindowsIdentity]::GetCurrent();\
        $principal = New-Object Security.Principal.WindowsPrincipal($identity);\
        $admin = New-Object Security.Principal.SecurityIdentifier('S-1-5-32-544');\
        $hyperv = New-Object Security.Principal.SecurityIdentifier('S-1-5-32-578');\
        ($principal.IsInRole($admin)) -or ($principal.IsInRole($hyperv))";

    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()
        .await;

    if let Ok(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        return stdout.trim().eq_ignore_ascii_case("true");
    }
    false
}


// ===== Linux Validation =====

#[cfg(target_os = "linux")]
#[allow(dead_code)]
async fn validate_linux_prerequisites() -> Result<()> {
    validate_linux_detailed().await.into_result()
}

#[cfg(target_os = "linux")]
async fn validate_linux_detailed() -> ValidationResult {
    info!("Validating Linux runtime prerequisites...");
    let mut result = ValidationResult::default();

    // Check 1: libkrunfw kernel firmware (dlopened at runtime by the statically
    // linked `krun` rlib). libkrun itself is compiled into nanosb as a Rust
    // rlib, so no libkrun.so is required on disk — but libkrunfw.so.5 must be
    // loadable via the dynamic linker.
    debug!("Checking for libkrunfw kernel firmware...");
    let libkrunfw_found = find_linux_libkrunfw();
    if let Some(path) = libkrunfw_found {
        debug!("Found libkrunfw at: {}", path);
    } else {
        result.add_error(
            "libkrunfw Kernel Firmware",
            "libkrunfw.so.5 not found in standard library paths",
            Some("Run: bash <(curl -fsSL https://github.com/nanosandboxai/nanosandbox/releases/latest/download/install.sh)".to_string()),
        );
    }

    // Check 2: KVM availability
    debug!("Checking KVM availability...");
    let kvm_path = Path::new("/dev/kvm");
    if !kvm_path.exists() {
        result.add_error(
            "KVM Device",
            "/dev/kvm does not exist",
            Some(
                "Enable KVM in BIOS/UEFI and load kvm module: modprobe kvm_intel (or kvm_amd)"
                    .to_string(),
            ),
        );
    } else {
        // Check permissions
        let can_access = std::fs::metadata(kvm_path)
            .map(|m| {
                use std::os::unix::fs::MetadataExt;
                let mode = m.mode();
                // Check if readable/writable by owner or group
                (mode & 0o660) != 0
            })
            .unwrap_or(false);

        if !can_access {
            // Try to actually open it
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(kvm_path)
            {
                Ok(_) => debug!("KVM access verified"),
                Err(e) => {
                    result.add_error(
                        "KVM Permissions",
                        format!("Cannot access /dev/kvm: {}", e),
                        Some("Add user to kvm group: sudo usermod -aG kvm $USER".to_string()),
                    );
                }
            }
        }
    }

    // Check 3: gvproxy (warning only - not required, TSI is the fallback)
    debug!("Checking for gvproxy...");
    if super::gvproxy::GvproxyManager::is_available() {
        if let Some(path) = super::gvproxy::GvproxyManager::find_binary() {
            debug!("Found gvproxy at {}", path.display());
            info!("gvproxy available - full outbound networking enabled");
        }
    } else {
        result.add_warning(
            "gvproxy not found - outbound networking from VMs will be limited. \
             Run: bash <(curl -fsSL https://github.com/nanosandboxai/nanosandbox/releases/latest/download/install.sh)".to_string(),
        );
    }

    // Log warnings
    for warning in &result.warnings {
        warn!("{}", warning);
    }

    result
}

/// Find the libkrunfw kernel firmware shared library on Linux.
///
/// This matches the soname that libkrun's Rust source dlopens at runtime
/// (`libkrunfw.so.5`, see `deps/libkrun/src/libkrun/src/lib.rs`).
#[cfg(target_os = "linux")]
fn find_linux_libkrunfw() -> Option<String> {
    const SONAME: &str = "libkrunfw.so.5";

    // Search ~/.nanosandbox/libs/ first (user-local install), then system dirs
    let mut search_dirs: Vec<String> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        search_dirs.push(format!("{}/.nanosandbox/libs", home));
    }
    search_dirs.extend([
        "/usr/lib".to_string(),
        "/usr/lib64".to_string(),
        "/usr/local/lib".to_string(),
        "/usr/lib/x86_64-linux-gnu".to_string(),
        "/usr/lib/aarch64-linux-gnu".to_string(),
    ]);

    for dir in &search_dirs {
        let candidate = format!("{}/{}", dir, SONAME);
        if Path::new(&candidate).exists() {
            return Some(candidate);
        }
    }
    None
}

// ===== macOS Validation =====

#[cfg(target_os = "macos")]
#[allow(dead_code)]
async fn validate_macos_prerequisites() -> Result<()> {
    validate_macos_detailed().await.into_result()
}

#[cfg(target_os = "macos")]
async fn validate_macos_detailed() -> ValidationResult {
    info!("Validating macOS runtime prerequisites...");
    let mut result = ValidationResult::default();

    // Check 1: Apple Silicon
    debug!("Checking architecture...");
    let arch = std::env::consts::ARCH;
    if arch != "aarch64" {
        result.add_error(
            "Architecture",
            format!(
                "Unsupported architecture: {}. Only Apple Silicon (aarch64) is supported.",
                arch
            ),
            None,
        );
    }

    // Check 2: libkrunfw kernel firmware (dlopened at runtime by the statically
    // linked `krun` rlib). libkrun itself is compiled into nanosb as a Rust
    // rlib, so no libkrun.dylib is required — but libkrunfw.5.dylib must be
    // discoverable via the same paths `preload_libkrunfw()` searches.
    debug!("Checking for libkrunfw kernel firmware...");
    if let Some(path) = find_macos_libkrunfw() {
        debug!("Found libkrunfw at {}", path);
    } else {
        result.add_error(
            "libkrunfw Kernel Firmware",
            "libkrunfw.5.dylib not found in ~/.nanosandbox/libs/, /opt/homebrew/lib, or /usr/local/lib",
            Some("Run: bash <(curl -fsSL https://github.com/nanosandboxai/nanosandbox/releases/latest/download/install.sh)".to_string()),
        );
    }

    // Check 3: Hypervisor.framework
    debug!("Checking Hypervisor.framework...");
    let hvf_available = check_hvf_available().await;
    if !hvf_available {
        result.add_error(
            "Hypervisor.framework",
            "Hypervisor.framework is not available or entitled",
            Some("Ensure you're running macOS 11+ on Apple Silicon".to_string()),
        );
    }

    // Check 4: gvproxy (warning only - not required, TSI is the fallback)
    debug!("Checking for gvproxy...");
    if super::gvproxy::GvproxyManager::is_available() {
        if let Some(path) = super::gvproxy::GvproxyManager::find_binary() {
            debug!("Found gvproxy at {}", path.display());
            info!("gvproxy available - full outbound networking enabled");
        }
    } else {
        result.add_warning(
            "gvproxy not found - outbound networking from VMs will be limited. \
             Run: bash <(curl -fsSL https://github.com/nanosandboxai/nanosandbox/releases/latest/download/install.sh)".to_string(),
        );
    }

    // Log warnings
    for warning in &result.warnings {
        warn!("{}", warning);
    }

    result
}

/// Find the libkrunfw kernel firmware dylib on macOS.
///
/// Searches the same directories as `runtime::libkrun::preload_libkrunfw()` so
/// that the doctor reports success exactly when the runtime will actually be
/// able to load the firmware at VM boot time.
#[cfg(target_os = "macos")]
fn find_macos_libkrunfw() -> Option<String> {
    // Search ~/.nanosandbox/libs/ first (user-local install), then system dirs
    let mut search_dirs: Vec<String> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        search_dirs.push(format!("{}/.nanosandbox/libs", home));
    }
    search_dirs.extend(["/opt/homebrew/lib".to_string(), "/usr/local/lib".to_string()]);

    for dir in &search_dirs {
        let path = format!("{}/libkrunfw.5.dylib", dir);
        if std::path::Path::new(&path).exists() {
            return Some(path);
        }
    }
    None
}

#[cfg(target_os = "macos")]
async fn check_hvf_available() -> bool {
    use tokio::process::Command;

    // Try to check if Hypervisor.framework is available
    // sysctl returns 1 if HVF is available
    let output = Command::new("sysctl")
        .args(["-n", "kern.hv_support"])
        .output()
        .await;

    if let Ok(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        return stdout.trim() == "1";
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validation_result_empty_is_ok() {
        let result = ValidationResult::default();
        assert!(result.is_ok());
    }

    #[test]
    fn test_validation_result_with_error() {
        let mut result = ValidationResult::default();
        result.add_error("test", "test error", None);
        assert!(!result.is_ok());
    }

    #[test]
    fn test_validation_result_with_warning_is_ok() {
        let mut result = ValidationResult::default();
        result.add_warning("test warning");
        assert!(result.is_ok());
    }

    #[test]
    fn test_prerequisite_error_display() {
        let error = PrerequisiteError {
            check: "TestCheck".to_string(),
            message: "Something failed".to_string(),
            fix_hint: Some("Do this to fix".to_string()),
        };
        let display = error.to_string();
        assert!(display.contains("TestCheck"));
        assert!(display.contains("Something failed"));
        assert!(display.contains("Do this to fix"));
    }

    #[tokio::test]
    async fn test_validate_detailed_returns_validation_result() {
        let result = super::validate_runtime_prerequisites_detailed().await;
        let _is_ok = result.is_ok();
        let _errors = &result.errors;
        let _warnings = &result.warnings;
    }
}
