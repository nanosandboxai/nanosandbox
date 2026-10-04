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
    #[cfg(target_os = "linux")]
    {
        validate_linux_detailed().await
    }

    #[cfg(target_os = "macos")]
    {
        validate_macos_detailed().await
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
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
