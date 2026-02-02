//! Runtime prerequisite validation
//!
//! This module provides validation checks for runtime prerequisites
//! on each supported platform.

use crate::error::{Error, PrerequisiteError, Result};
use tracing::{debug, info, warn};

#[cfg(target_os = "linux")]
use std::path::Path;

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
            Err(Error::PrerequisiteChecksFailed(self.errors))
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
    #[cfg(target_os = "windows")]
    {
        validate_windows_prerequisites().await
    }

    #[cfg(target_os = "linux")]
    {
        validate_linux_prerequisites().await
    }

    #[cfg(target_os = "macos")]
    {
        validate_macos_prerequisites().await
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        Err(Error::UnsupportedPlatform {
            platform: std::env::consts::OS.to_string(),
        })
    }
}

// ===== Windows Validation =====

#[cfg(target_os = "windows")]
async fn validate_windows_prerequisites() -> Result<()> {
    info!("Validating Windows runtime prerequisites...");
    let mut result = ValidationResult::default();

    // Check 1: Windows Containers feature
    debug!("Checking Windows Containers feature...");
    let containers_enabled = check_windows_feature("Containers").await;
    if !containers_enabled {
        result.add_error(
            "Windows Containers",
            "Windows Containers feature is not enabled",
            Some(
                "Run: Enable-WindowsOptionalFeature -Online -FeatureName Containers -All"
                    .to_string(),
            ),
        );
    }

    // Check 2: Runtime binary (runhcs.exe)
    debug!("Checking for runtime binary...");
    let runtime_found = find_windows_runtime().await.is_some();
    if !runtime_found {
        result.add_error(
            "Runtime Binary",
            "No Windows container runtime found (runhcs.exe, ctr.exe)",
            Some("Install Docker Desktop or Windows Container tools".to_string()),
        );
    }

    // Check 3: HCS service running
    debug!("Checking HCS service...");
    let hcs_running = check_hcs_service().await;
    if !hcs_running {
        result.add_error(
            "HCS Service",
            "Host Compute Service (vmcompute) is not running",
            Some("Run: Start-Service vmcompute".to_string()),
        );
    }

    // Check 4: Hyper-V (optional, for Hyper-V isolation)
    debug!("Checking Hyper-V feature...");
    let hyperv_enabled = check_windows_feature("Microsoft-Hyper-V").await;
    if !hyperv_enabled {
        result.add_warning(
            "Hyper-V is not enabled. Only process isolation will be available.".to_string(),
        );
    }

    // Log warnings
    for warning in &result.warnings {
        warn!("{}", warning);
    }

    result.into_result()
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

/// Find the Windows container runtime binary (runhcs, ctr, or hcsdiag)
#[cfg(target_os = "windows")]
pub async fn find_windows_runtime() -> Option<String> {
    use tokio::process::Command;

    // Try to find runtime binaries in order of preference
    for binary in &["runhcs.exe", "ctr.exe", "hcsdiag.exe"] {
        let output = Command::new("where").arg(binary).output().await;

        if let Ok(out) = output {
            if out.status.success() {
                let path = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !path.is_empty() {
                    debug!("Found Windows runtime: {}", path);
                    return Some(path);
                }
            }
        }
    }
    None
}

// ===== Linux Validation =====

#[cfg(target_os = "linux")]
async fn validate_linux_prerequisites() -> Result<()> {
    info!("Validating Linux runtime prerequisites...");
    let mut result = ValidationResult::default();

    // Check 1: Runtime binary (krun or crun)
    debug!("Checking for runtime binary...");
    let runtime = find_linux_runtime().await;
    if runtime.is_none() {
        result.add_error(
            "Runtime Binary",
            "No OCI runtime found (krun, crun)",
            Some(
                "Install crun with libkrun support: https://github.com/containers/crun".to_string(),
            ),
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

    // Log warnings
    for warning in &result.warnings {
        warn!("{}", warning);
    }

    result.into_result()
}

/// Find the Linux OCI runtime binary (krun or crun)
#[cfg(target_os = "linux")]
pub async fn find_linux_runtime() -> Option<String> {
    use tokio::process::Command;

    // Try krun first (preferred for VM isolation), then crun
    for binary in &["krun", "crun"] {
        let output = Command::new("which").arg(binary).output().await;

        if let Ok(out) = output {
            if out.status.success() {
                let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !path.is_empty() {
                    debug!("Found Linux runtime: {}", path);
                    return Some(path);
                }
            }
        }
    }
    None
}

// ===== macOS Validation =====

#[cfg(target_os = "macos")]
async fn validate_macos_prerequisites() -> Result<()> {
    use tokio::process::Command;

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

    // Check 2: krunvm binary
    debug!("Checking for krunvm binary...");
    let krunvm = find_macos_runtime().await;
    if krunvm.is_none() {
        result.add_error(
            "Runtime Binary",
            "krunvm not found",
            Some("Install via Homebrew: brew tap slp/krun && brew install krunvm".to_string()),
        );
    }

    // Check 3: Hypervisor.framework entitlement
    // This is harder to check directly, but krunvm will fail if not available
    debug!("Checking Hypervisor.framework...");
    let hvf_available = check_hvf_available().await;
    if !hvf_available {
        result.add_error(
            "Hypervisor.framework",
            "Hypervisor.framework is not available or entitled",
            Some("Ensure you're running macOS 11+ on Apple Silicon".to_string()),
        );
    }

    // Log warnings
    for warning in &result.warnings {
        warn!("{}", warning);
    }

    result.into_result()
}

#[cfg(target_os = "macos")]
pub async fn find_macos_runtime() -> Option<String> {
    use tokio::process::Command;

    let output = Command::new("which").arg("krunvm").output().await;

    if let Ok(out) = output {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                debug!("Found macOS runtime: {}", path);
                return Some(path);
            }
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
}
