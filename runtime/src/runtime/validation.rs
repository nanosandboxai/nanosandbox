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
    validate_runtime_prerequisites_detailed().await.into_result()
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
    info!("Validating Windows containerd runtime prerequisites...");
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

    // Check 2: containerd daemon running
    debug!("Checking containerd service...");
    let containerd_running = check_containerd_service().await;
    if !containerd_running {
        result.add_error(
            "containerd Service",
            "containerd is not running or not reachable",
            Some(
                "Install containerd and start the service:\n\
                 1. Download from https://github.com/containerd/containerd/releases\n\
                 2. Extract to C:\\Program Files\\containerd\n\
                 3. Run: containerd.exe --register-service\n\
                 4. Run: Start-Service containerd"
                    .to_string(),
            ),
        );
    }

    // Check 3: containerd-shim-runhcs-v1.exe available
    debug!("Checking containerd-shim-runhcs-v1...");
    let shim_found = find_containerd_shim().await.is_some();
    if !shim_found {
        result.add_error(
            "runhcs Shim",
            "containerd-shim-runhcs-v1.exe not found",
            Some(
                "Build and install the runhcs shim from hcsshim:\n\
                 1. Clone https://github.com/microsoft/hcsshim\n\
                 2. Run: go build -o containerd-shim-runhcs-v1.exe ./cmd/containerd-shim-runhcs-v1\n\
                 3. Copy to C:\\Program Files\\containerd\\ (same dir as containerd.exe)"
                    .to_string(),
            ),
        );
    }

    // Check 4: HCS service running
    debug!("Checking HCS service...");
    let hcs_running = check_hcs_service().await;
    if !hcs_running {
        result.add_error(
            "HCS Service",
            "Host Compute Service (vmcompute) is not running",
            Some("Run: Start-Service vmcompute".to_string()),
        );
    }

    // Check 5: Hyper-V (optional, for Hyper-V isolation)
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
            let ctr_check = Command::new("ctr")
                .args(["version"])
                .output()
                .await;
            
            if let Ok(ctr_out) = ctr_check {
                return ctr_out.status.success();
            }
        }
    }
    
    // Also try connecting directly in case containerd runs without Windows service
    let ctr_check = Command::new("ctr")
        .args(["version"])
        .output()
        .await;
    
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

    let output = Command::new("where")
        .arg("containerd.exe")
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

    // Check 1: libkrun library
    debug!("Checking for libkrun library...");
    let libkrun_found = find_linux_libkrun();
    if libkrun_found.is_none() {
        result.add_error(
            "libkrun Library",
            "libkrun.so not found",
            Some("Run: ./scripts/install/linux.sh".to_string()),
        );
    } else {
        debug!("Found libkrun at: {}", libkrun_found.unwrap());
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
             Run: ./scripts/install/linux.sh".to_string(),
        );
    }

    // Log warnings
    for warning in &result.warnings {
        warn!("{}", warning);
    }

    result
}

/// Find the libkrun shared library on Linux
#[cfg(target_os = "linux")]
fn find_linux_libkrun() -> Option<String> {
    let search_paths = [
        "/usr/lib/libkrun.so",
        "/usr/lib64/libkrun.so",
        "/usr/local/lib/libkrun.so",
        "/usr/lib/x86_64-linux-gnu/libkrun.so",
        "/usr/lib/aarch64-linux-gnu/libkrun.so",
    ];

    for path in &search_paths {
        if Path::new(path).exists() {
            return Some(path.to_string());
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

    // Check 2: libkrun library
    debug!("Checking for libkrun library...");
    if !std::path::Path::new("/opt/homebrew/lib/libkrun.dylib").exists() {
        result.add_error(
            "libkrun Library",
            "libkrun.dylib not found at /opt/homebrew/lib/",
            Some("Run: ./scripts/install/macos.sh".to_string()),
        );
    } else {
        debug!("Found libkrun at /opt/homebrew/lib/libkrun.dylib");
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
             Run: ./scripts/install/macos.sh".to_string(),
        );
    }

    // Log warnings
    for warning in &result.warnings {
        warn!("{}", warning);
    }

    result
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
