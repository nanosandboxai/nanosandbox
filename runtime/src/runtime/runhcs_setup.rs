//! runhcs and wclayer setup and auto-download module
//!
//! **DEPRECATED**: This module is deprecated. The Windows runtime now uses containerd
//! with containerd-shim-runhcs-v1 for container execution. containerd handles image
//! pulling and layer management via its snapshotter, avoiding the `ProcessBaseLayer`
//! idempotency issues that plagued direct wclayer usage.
//!
//! This module is kept for reference only and may be removed in a future version.
//!
//! # Legacy Information
//!
//! This module previously handled automatic downloading and setup of Windows container tools
//! from the hcsshim project:
//!
//! - `runhcs.exe` - Standalone OCI runtime for Windows containers (works directly with HCS)
//! - `wclayer.exe` - Tool to import OCI tar layers into Windows container format
//!
//! # Migration
//!
//! Use `ContainerdWindowsRuntime` instead, which:
//! - Uses containerd for image pulling and snapshot management
//! - Avoids the flakey `wclayer import` path
//! - Uses `containerd-shim-runhcs-v1` as the Runtime v2 shim

#![cfg(target_os = "windows")]
#![deprecated(
    since = "0.2.0",
    note = "Use ContainerdWindowsRuntime instead. Direct runhcs/wclayer usage is deprecated."
)]
#![allow(deprecated)]

use crate::error::{Error, Result};
use std::path::PathBuf;
use tokio::fs;
use tracing::{debug, info, warn};

/// Version of hcsshim tools to download
const HCSSHIM_VERSION: &str = "0.12.9";

/// URL template for downloading runhcs.exe from nanosandbox releases
const RUNHCS_DOWNLOAD_URL: &str = 
    "https://github.com/nanosandboxai/runtime/releases/download/runhcs-v{VERSION}/runhcs.exe";

/// URL template for downloading wclayer.exe from nanosandbox releases
const WCLAYER_DOWNLOAD_URL: &str = 
    "https://github.com/nanosandboxai/runtime/releases/download/runhcs-v{VERSION}/wclayer.exe";

/// Alternative: Download from hcsshim releases (if available)
const HCSSHIM_RUNHCS_URL: &str = 
    "https://github.com/microsoft/hcsshim/releases/download/v{VERSION}/runhcs.exe";

/// Alternative: Download wclayer from hcsshim releases (if available)
const HCSSHIM_WCLAYER_URL: &str = 
    "https://github.com/microsoft/hcsshim/releases/download/v{VERSION}/wclayer.exe";

/// Get the nanosandbox bin directory
fn get_bin_dir() -> Result<PathBuf> {
    let bin_dir = dirs::data_local_dir()
        .ok_or_else(|| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Cannot find local data directory",
            ))
        })?
        .join("nanosandbox")
        .join("bin");
    Ok(bin_dir)
}

/// Get the path to runhcs.exe
pub fn get_runhcs_path() -> Result<PathBuf> {
    Ok(get_bin_dir()?.join("runhcs.exe"))
}

/// Get the path to wclayer.exe
pub fn get_wclayer_path() -> Result<PathBuf> {
    Ok(get_bin_dir()?.join("wclayer.exe"))
}

/// Check if runhcs.exe exists and is executable
pub async fn runhcs_exists() -> bool {
    if let Ok(path) = get_runhcs_path() {
        if path.exists() {
            // Verify it's actually runhcs by running --version
            let output = tokio::process::Command::new(&path)
                .arg("--version")
                .output()
                .await;
            
            if let Ok(out) = output {
                return out.status.success();
            }
        }
    }
    false
}

/// Ensure runhcs.exe is available, downloading if necessary
///
/// Returns the path to the runhcs.exe binary.
///
/// # Download Strategy
///
/// 1. First checks if runhcs.exe already exists
/// 2. Tries to download from nanosandbox releases
/// 3. Falls back to checking system PATH
///
/// # Errors
///
/// Returns an error if:
/// - Cannot create the bin directory
/// - Download fails and no system runhcs.exe is available
pub async fn ensure_runhcs() -> Result<PathBuf> {
    let runhcs_path = get_runhcs_path()?;
    
    // Check if already exists
    if runhcs_exists().await {
        debug!("runhcs.exe found at: {}", runhcs_path.display());
        return Ok(runhcs_path);
    }
    
    // Check system PATH first
    if let Some(system_path) = find_system_runhcs().await {
        info!("Using system runhcs.exe: {}", system_path);
        return Ok(PathBuf::from(system_path));
    }
    
    // Try to download
    info!("runhcs.exe not found, attempting to download...");
    
    // Create bin directory
    let bin_dir = get_bin_dir()?;
    fs::create_dir_all(&bin_dir).await.map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to create bin directory: {}", e),
        ))
    })?;
    
    // Try download from nanosandbox releases
    match download_runhcs(&runhcs_path).await {
        Ok(_) => {
            info!("runhcs.exe downloaded successfully to: {}", runhcs_path.display());
            Ok(runhcs_path)
        }
        Err(e) => {
            warn!("Failed to download runhcs.exe: {}", e);
            Err(Error::RuntimeBinaryNotFound {
                binary: "runhcs.exe".to_string(),
                install_hint: format!(
                    "Download runhcs.exe from hcsshim releases or build from source:\n\
                     git clone https://github.com/microsoft/hcsshim\n\
                     cd hcsshim && go build -o runhcs.exe ./cmd/runhcs\n\
                     Copy runhcs.exe to: {}",
                    runhcs_path.display()
                ),
            })
        }
    }
}

// ===== wclayer.exe Setup =====

/// Check if wclayer.exe exists and is executable
pub async fn wclayer_exists() -> bool {
    if let Ok(path) = get_wclayer_path() {
        if path.exists() {
            // Verify it's actually wclayer by running --help
            let output = tokio::process::Command::new(&path)
                .arg("--help")
                .output()
                .await;
            
            if let Ok(out) = output {
                // wclayer --help returns exit code 0 and contains usage info
                return out.status.success() || 
                    String::from_utf8_lossy(&out.stdout).contains("wclayer");
            }
        }
    }
    false
}

/// Ensure wclayer.exe is available, downloading if necessary
///
/// wclayer.exe is used to import OCI tar layers into Windows container format.
/// It's part of the hcsshim project and converts standard OCI layers to the
/// format required by Windows containers.
///
/// Returns the path to the wclayer.exe binary.
///
/// # Errors
///
/// Returns an error if:
/// - Cannot create the bin directory
/// - Download fails and no system wclayer.exe is available
pub async fn ensure_wclayer() -> Result<PathBuf> {
    let wclayer_path = get_wclayer_path()?;
    
    // Check if already exists
    if wclayer_exists().await {
        debug!("wclayer.exe found at: {}", wclayer_path.display());
        return Ok(wclayer_path);
    }
    
    // Check system PATH first
    if let Some(system_path) = find_system_wclayer().await {
        info!("Using system wclayer.exe: {}", system_path);
        return Ok(PathBuf::from(system_path));
    }
    
    // Try to download
    info!("wclayer.exe not found, attempting to download...");
    
    // Create bin directory
    let bin_dir = get_bin_dir()?;
    fs::create_dir_all(&bin_dir).await.map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("Failed to create bin directory: {}", e),
        ))
    })?;
    
    // Try download from nanosandbox releases
    match download_wclayer(&wclayer_path).await {
        Ok(_) => {
            info!("wclayer.exe downloaded successfully to: {}", wclayer_path.display());
            Ok(wclayer_path)
        }
        Err(e) => {
            warn!("Failed to download wclayer.exe: {}", e);
            Err(Error::RuntimeBinaryNotFound {
                binary: "wclayer.exe".to_string(),
                install_hint: format!(
                    "Download wclayer.exe from hcsshim releases or build from source:\n\
                     git clone https://github.com/microsoft/hcsshim\n\
                     cd hcsshim && go build -o wclayer.exe ./cmd/wclayer\n\
                     Copy wclayer.exe to: {}",
                    wclayer_path.display()
                ),
            })
        }
    }
}

/// Find wclayer.exe in system PATH
async fn find_system_wclayer() -> Option<String> {
    let output = tokio::process::Command::new("where")
        .arg("wclayer.exe")
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
    None
}

/// Download wclayer.exe from GitHub releases
async fn download_wclayer(dest_path: &PathBuf) -> Result<()> {
    // Try nanosandbox releases first
    let url = WCLAYER_DOWNLOAD_URL.replace("{VERSION}", HCSSHIM_VERSION);
    
    info!("Downloading wclayer.exe from: {}", url);
    
    match download_file(&url, dest_path).await {
        Ok(_) => return Ok(()),
        Err(e) => {
            warn!("Failed to download from nanosandbox releases: {}", e);
        }
    }
    
    // Try hcsshim releases
    let hcsshim_url = HCSSHIM_WCLAYER_URL.replace("{VERSION}", HCSSHIM_VERSION);
    info!("Trying hcsshim releases: {}", hcsshim_url);
    
    download_file(&hcsshim_url, dest_path).await
}

/// Find runhcs.exe in system PATH
async fn find_system_runhcs() -> Option<String> {
    let output = tokio::process::Command::new("where")
        .arg("runhcs.exe")
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
    None
}

/// Download runhcs.exe from GitHub releases
async fn download_runhcs(dest_path: &PathBuf) -> Result<()> {
    // Try nanosandbox releases first
    let url = RUNHCS_DOWNLOAD_URL.replace("{VERSION}", HCSSHIM_VERSION);
    
    info!("Downloading runhcs.exe from: {}", url);
    
    match download_file(&url, dest_path).await {
        Ok(_) => return Ok(()),
        Err(e) => {
            warn!("Failed to download from nanosandbox releases: {}", e);
        }
    }
    
    // Try hcsshim releases
    let hcsshim_url = HCSSHIM_RUNHCS_URL.replace("{VERSION}", HCSSHIM_VERSION);
    info!("Trying hcsshim releases: {}", hcsshim_url);
    
    download_file(&hcsshim_url, dest_path).await
}

/// Download a file from URL to destination
async fn download_file(url: &str, dest_path: &PathBuf) -> Result<()> {
    // Use PowerShell's Invoke-WebRequest for downloading
    // This avoids adding reqwest as a dependency
    let output = tokio::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "Invoke-WebRequest -Uri '{}' -OutFile '{}' -UseBasicParsing",
                url,
                dest_path.display()
            ),
        ])
        .output()
        .await
        .map_err(|e| Error::Io(e))?;
    
    if output.status.success() {
        // Verify the download
        if dest_path.exists() {
            let metadata = std::fs::metadata(dest_path).map_err(Error::Io)?;
            if metadata.len() > 1000 {
                // Reasonable minimum size for an executable
                return Ok(());
            }
        }
    }
    
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(Error::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        format!("Download failed: {}", stderr),
    )))
}

/// Get the version of installed runhcs
pub async fn get_runhcs_version() -> Option<String> {
    let runhcs_path = match ensure_runhcs().await {
        Ok(p) => p,
        Err(_) => return None,
    };
    
    let output = tokio::process::Command::new(&runhcs_path)
        .arg("--version")
        .output()
        .await;
    
    if let Ok(out) = output {
        if out.status.success() {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
            return Some(version);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_get_bin_dir() {
        let bin_dir = get_bin_dir();
        assert!(bin_dir.is_ok());
        let path = bin_dir.unwrap();
        assert!(path.ends_with("nanosandbox\\bin") || path.ends_with("nanosandbox/bin"));
    }
    
    #[test]
    fn test_get_runhcs_path() {
        let path = get_runhcs_path();
        assert!(path.is_ok());
        let path = path.unwrap();
        assert!(path.ends_with("runhcs.exe"));
    }
}
