//! Runtime prerequisite validation
//!
//! This module provides validation checks for runtime prerequisites
//! on each supported platform.

use crate::error::{Error, Result};
use tracing::{debug, info, warn};

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
    info!("Validating Windows libkrun (WHPX) runtime prerequisites...");
    let mut result = ValidationResult::default();

    // Check 1: Windows Hypervisor Platform feature
    debug!("Checking Windows Hypervisor Platform feature...");
    let whpx_enabled = check_windows_feature("HypervisorPlatform").await;
    if !whpx_enabled {
        result.add_error(
            "Windows Hypervisor Platform",
            "WHPX feature is not enabled",
            Some(
                "Run: Enable-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform -All\n\
                 Then restart the computer."
                    .to_string(),
            ),
        );
    }

    // Check 2: Hyper-V hypervisor (required for WHPX)
    debug!("Checking Hyper-V...");
    let hyperv_enabled = check_windows_feature("Microsoft-Hyper-V-Hypervisor").await;
    if !hyperv_enabled {
        result.add_warning(
            "Hyper-V hypervisor not detected. WHPX requires the hypervisor to be active.\n\
             Run: Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -All"
                .to_string(),
        );
    }

    // Check 3: krun.dll loadable
    debug!("Checking krun.dll availability...");
    let krun_found = check_dll_loadable("krun.dll").await;
    if !krun_found {
        result.add_error(
            "krun.dll",
            "krun.dll (libkrun) not found in PATH or current directory",
            Some(
                "Build libkrun-win and place krun.dll in your PATH:\n\
                 1. cd C:\\libkrun-win\n\
                 2. .\\scripts\\build-windows.ps1\n\
                 3. Copy target\\debug\\krun.dll to the CLI directory or add to PATH"
                    .to_string(),
            ),
        );
    }

    // Check 4: libkrunfw.dll loadable
    debug!("Checking libkrunfw.dll availability...");
    let krunfw_found = check_dll_loadable("libkrunfw.dll").await;
    if !krunfw_found {
        result.add_error(
            "libkrunfw.dll",
            "libkrunfw.dll (kernel firmware) not found in PATH or current directory",
            Some(
                "Build libkrunfw-win and place libkrunfw.dll alongside krun.dll:\n\
                 The build-windows.ps1 script creates both DLLs automatically."
                    .to_string(),
            ),
        );
    }

    // Log warnings
    for warning in &result.warnings {
        warn!("{}", warning);
    }

    result
}

#[cfg(target_os = "windows")]
async fn check_windows_feature(feature: &str) -> bool {
    use tokio::process::Command;

    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "(Get-WindowsOptionalFeature -Online -FeatureName {}).State -eq 'Enabled'",
                feature
            ),
        ])
        .output()
        .await;

    if let Ok(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        return stdout.trim().eq_ignore_ascii_case("true");
    }
    false
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

/// Check if a DLL is loadable (exists in PATH, current dir, or system dirs)
#[cfg(target_os = "windows")]
async fn check_dll_loadable(dll_name: &str) -> bool {
    use tokio::process::Command;

    // Use PowerShell to check if the DLL exists in common locations
    let script = format!(
        "$paths = @('.', $env:PATH -split ';') | Where-Object {{ $_ -ne '' }}; \
         foreach ($p in $paths) {{ if (Test-Path (Join-Path $p '{}')) {{ Write-Output 'true'; return }} }}; \
         Write-Output 'false'",
        dll_name
    );

    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .await;

    if let Ok(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        return stdout.trim().eq_ignore_ascii_case("true");
    }
    false
}

/// Check if containerd service is running and reachable
#[cfg(target_os = "windows")]
async fn check_containerd_service() -> bool {
    use tokio::process::Command;

    // First check if containerd service is running
    let service_check = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "(Get-Service containerd -ErrorAction SilentlyContinue).Status -eq 'Running'",
        ])
        .output()
        .await;

    if let Ok(out) = service_check {
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.trim().eq_ignore_ascii_case("true") {
            // Service is running, verify we can connect via ctr
            let ctr_check = Command::new("ctr").args(["version"]).output().await;

            if let Ok(ctr_out) = ctr_check {
                return ctr_out.status.success();
            }
        }
    }

    // Also try connecting directly in case containerd runs without Windows service
    let ctr_check = Command::new("ctr").args(["version"]).output().await;

    matches!(ctr_check, Ok(out) if out.status.success())
}

/// Find the containerd-shim-runhcs-v1.exe binary
#[cfg(target_os = "windows")]
pub async fn find_containerd_shim() -> Option<String> {
    use tokio::process::Command;

    // Check in PATH
    let output = Command::new("where")
        .arg("containerd-shim-runhcs-v1.exe")
        .output()
        .await;

    if let Ok(out) = output {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !path.is_empty() {
                debug!("Found containerd-shim-runhcs-v1: {}", path);
                return Some(path);
            }
        }
    }

    // Check common installation directories
    let common_paths = [
        r"C:\Program Files\containerd\containerd-shim-runhcs-v1.exe",
        r"C:\containerd\containerd-shim-runhcs-v1.exe",
    ];

    for path in &common_paths {
        if std::path::Path::new(path).exists() {
            debug!("Found containerd-shim-runhcs-v1: {}", path);
            return Some(path.to_string());
        }
    }

    // Check next to containerd.exe
    if let Some(containerd_path) = find_containerd_exe().await {
        let shim_path = std::path::Path::new(&containerd_path)
            .parent()
            .map(|p| p.join("containerd-shim-runhcs-v1.exe"));

        if let Some(shim) = shim_path {
            if shim.exists() {
                debug!("Found containerd-shim-runhcs-v1: {}", shim.display());
                return Some(shim.to_string_lossy().to_string());
            }
        }
    }

    None
}

/// Find the containerd.exe binary
#[cfg(target_os = "windows")]
async fn find_containerd_exe() -> Option<String> {
    use tokio::process::Command;

    let output = Command::new("where").arg("containerd.exe").output().await;

    if let Ok(out) = output {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !path.is_empty() {
                return Some(path);
            }
        }
    }

    // Check common paths
    let common_paths = [
        r"C:\Program Files\containerd\containerd.exe",
        r"C:\containerd\containerd.exe",
    ];

    for path in &common_paths {
        if std::path::Path::new(path).exists() {
            return Some(path.to_string());
        }
    }

    None
}

/// Find the Windows container runtime (containerd + shim)
///
/// Returns the path to ctr.exe if containerd infrastructure is available.
#[cfg(target_os = "windows")]
pub async fn find_windows_runtime() -> Option<String> {
    use tokio::process::Command;

    // Check for ctr.exe (containerd CLI) in PATH
    let output = Command::new("where").arg("ctr.exe").output().await;

    if let Ok(out) = output {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !path.is_empty() {
                debug!("Found containerd CLI: {}", path);
                return Some(path);
            }
        }
    }

    // Check common installation paths
    let common_paths = [
        r"C:\Program Files\containerd\ctr.exe",
        r"C:\containerd\ctr.exe",
    ];

    for path in &common_paths {
        if std::path::Path::new(path).exists() {
            debug!("Found containerd CLI: {}", path);
            return Some(path.to_string());
        }
    }

    None
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
            Some("Run: bash <(curl -fsSL https://github.com/nanosandboxai/cli/releases/latest/download/install.sh)".to_string()),
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
             Run: bash <(curl -fsSL https://github.com/nanosandboxai/cli/releases/latest/download/install.sh)".to_string(),
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
    let search_dirs = [
        "/usr/lib",
        "/usr/lib64",
        "/usr/local/lib",
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
    ];

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
            "libkrunfw.5.dylib not found in /opt/homebrew/lib or /usr/local/lib",
            Some("Run: bash <(curl -fsSL https://github.com/nanosandboxai/cli/releases/latest/download/install.sh)".to_string()),
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
             Run: bash <(curl -fsSL https://github.com/nanosandboxai/cli/releases/latest/download/install.sh)".to_string(),
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
    const SEARCH_DIRS: &[&str] = &["/opt/homebrew/lib", "/usr/local/lib"];
    for dir in SEARCH_DIRS {
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
