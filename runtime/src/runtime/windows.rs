//! Windows Container Runtime Backend
//!
//! This backend uses Windows Containers (HCS - Host Compute Service) for
//! container execution on Windows. Supports both process isolation and
//! Hyper-V isolation modes.
//!
//! # Prerequisites
//!
//! - Windows 10/11 Pro, Enterprise, or Windows Server 2016+
//! - Containers feature enabled
//! - For Hyper-V isolation: Hyper-V feature enabled
//! - HCS service (vmcompute) running
//!
//! # Enable Prerequisites
//!
//! ```powershell
//! # Enable Containers feature
//! Enable-WindowsOptionalFeature -Online -FeatureName Containers -All
//!
//! # Enable Hyper-V (optional, for Hyper-V isolation)
//! Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -All
//!
//! # Start HCS service
//! Start-Service vmcompute
//! ```

use super::validation::find_windows_runtime;
use super::ExecOutput;
use crate::config::SandboxConfig;
use crate::error::{Error, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{debug, info};

/// Windows isolation mode for containers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowsIsolation {
    /// Process isolation - faster, shares kernel with host
    /// Suitable for same-OS-version containers
    #[default]
    Process,
    /// Hyper-V isolation - each container runs in lightweight VM
    /// Provides stronger isolation, supports different OS versions
    HyperV,
}

/// Windows Container Runtime using HCS
pub struct WindowsContainerRuntime {
    /// Path to the runtime binary (runhcs or ctr)
    binary_path: String,
    /// Runtime root directory for state
    root_dir: PathBuf,
    /// Isolation mode
    isolation: WindowsIsolation,
}

impl WindowsContainerRuntime {
    /// Create a new Windows Container runtime
    ///
    /// This will fail if prerequisites are not met.
    pub async fn new() -> Result<Self> {
        let binary_path =
            find_windows_runtime()
                .await
                .ok_or_else(|| Error::RuntimeBinaryNotFound {
                    binary: "runhcs.exe".to_string(),
                    install_hint: "Install Docker Desktop or Windows Container tools".to_string(),
                })?;

        let root_dir = Self::default_root_dir()?;

        info!("Windows Container runtime initialized: {}", binary_path);

        Ok(Self {
            binary_path,
            root_dir,
            isolation: WindowsIsolation::default(),
        })
    }

    /// Create with specific isolation mode
    pub async fn with_isolation(isolation: WindowsIsolation) -> Result<Self> {
        let mut runtime = Self::new().await?;
        runtime.isolation = isolation;
        Ok(runtime)
    }

    /// Get default root directory for runtime state
    fn default_root_dir() -> Result<PathBuf> {
        let root = dirs::data_local_dir()
            .ok_or_else(|| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Cannot find local data directory",
                ))
            })?
            .join("nanosandbox")
            .join("runtime");

        std::fs::create_dir_all(&root)?;
        Ok(root)
    }

    /// Get the runtime name
    pub fn name(&self) -> &str {
        "windows-containers"
    }

    /// Get the isolation mode
    pub fn isolation(&self) -> WindowsIsolation {
        self.isolation
    }

    /// Create a Windows container from OCI bundle
    pub async fn create(
        &self,
        id: &str,
        _config: &SandboxConfig,
        bundle_path: Option<&Path>,
    ) -> Result<()> {
        let bundle = bundle_path.ok_or_else(|| {
            Error::SandboxCreationFailed("Windows runtime requires bundle path".to_string())
        })?;

        let mut args = vec![
            "--root".to_string(),
            self.root_dir.to_string_lossy().to_string(),
            "create".to_string(),
            "--bundle".to_string(),
            bundle.to_string_lossy().to_string(),
        ];

        // Add isolation mode for Hyper-V
        if self.isolation == WindowsIsolation::HyperV {
            args.push("--isolation".to_string());
            args.push("hyperv".to_string());
        }

        args.push(id.to_string());

        debug!(
            "Creating Windows container: {} {:?}",
            self.binary_path, args
        );

        let output = Command::new(&self.binary_path)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if output.status.success() {
            info!("Created Windows container: {}", id);
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(Error::SandboxCreationFailed(format!(
                "Failed to create container: {}",
                stderr
            )))
        }
    }

    /// Start a Windows container
    pub async fn start(&self, id: &str) -> Result<()> {
        let output = Command::new(&self.binary_path)
            .args(["--root", &self.root_dir.to_string_lossy(), "start", id])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if output.status.success() {
            info!("Started Windows container: {}", id);
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(Error::SandboxCreationFailed(format!(
                "Failed to start container: {}",
                stderr
            )))
        }
    }

    /// Execute a command in a running Windows container
    pub async fn exec(
        &self,
        id: &str,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
    ) -> Result<ExecOutput> {
        let mut exec_args: Vec<String> = vec![
            "--root".to_string(),
            self.root_dir.to_string_lossy().to_string(),
            "exec".to_string(),
        ];

        if let Some(cwd) = workdir {
            exec_args.push("--cwd".to_string());
            exec_args.push(cwd.to_string());
        }

        for (key, value) in env {
            exec_args.push("--env".to_string());
            exec_args.push(format!("{}={}", key, value));
        }

        exec_args.push(id.to_string());
        exec_args.push(command.to_string());
        exec_args.extend(args.iter().map(|s| s.to_string()));

        debug!(
            "Executing in Windows container: {} {:?}",
            self.binary_path, exec_args
        );

        let output = Command::new(&self.binary_path)
            .args(&exec_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        Ok(ExecOutput {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    /// Execute with streaming output
    pub async fn exec_stream<F>(
        &self,
        id: &str,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
        on_output: F,
    ) -> Result<i32>
    where
        F: Fn(&str, bool) + Send + Sync,
    {
        let mut exec_args: Vec<String> = vec![
            "--root".to_string(),
            self.root_dir.to_string_lossy().to_string(),
            "exec".to_string(),
        ];

        if let Some(cwd) = workdir {
            exec_args.push("--cwd".to_string());
            exec_args.push(cwd.to_string());
        }

        for (key, value) in env {
            exec_args.push("--env".to_string());
            exec_args.push(format!("{}={}", key, value));
        }

        exec_args.push(id.to_string());
        exec_args.push(command.to_string());
        exec_args.extend(args.iter().map(|s| s.to_string()));

        debug!(
            "Executing (streaming) in Windows container: {} {:?}",
            self.binary_path, exec_args
        );

        let mut child = Command::new(&self.binary_path)
            .args(&exec_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let mut stdout_reader = BufReader::new(stdout).lines();
        let mut stderr_reader = BufReader::new(stderr).lines();

        loop {
            tokio::select! {
                line = stdout_reader.next_line() => {
                    match line {
                        Ok(Some(text)) => on_output(&text, false),
                        Ok(None) => break,
                        Err(e) => {
                            debug!("Error reading stdout: {}", e);
                            break;
                        }
                    }
                }
                line = stderr_reader.next_line() => {
                    match line {
                        Ok(Some(text)) => on_output(&text, true),
                        Ok(None) => {},
                        Err(e) => {
                            debug!("Error reading stderr: {}", e);
                        }
                    }
                }
            }
        }

        // Drain remaining stderr
        while let Ok(Some(text)) = stderr_reader.next_line().await {
            on_output(&text, true);
        }

        let status = child.wait().await?;
        Ok(status.code().unwrap_or(-1))
    }

    /// Stop a Windows container
    pub async fn stop(&self, id: &str) -> Result<()> {
        debug!("Stopping Windows container: {}", id);

        let output = Command::new(&self.binary_path)
            .args([
                "--root",
                &self.root_dir.to_string_lossy(),
                "kill",
                id,
                "SIGTERM",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            debug!(
                "Kill returned non-zero (may be expected): {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(())
    }

    /// Destroy a Windows container
    pub async fn destroy(&self, id: &str) -> Result<()> {
        debug!("Destroying Windows container: {}", id);

        let output = Command::new(&self.binary_path)
            .args([
                "--root",
                &self.root_dir.to_string_lossy(),
                "delete",
                "--force",
                id,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            debug!(
                "Delete returned non-zero (may be expected): {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        info!("Destroyed Windows container: {}", id);
        Ok(())
    }
}

/// Generate Windows-specific OCI runtime configuration
pub fn generate_windows_oci_config(
    config: &SandboxConfig,
    rootfs_path: &Path,
    layer_folders: &[PathBuf],
    isolation: WindowsIsolation,
) -> serde_json::Value {
    use serde_json::json;

    let env: Vec<String> = config
        .env
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .chain(default_windows_env())
        .collect();

    // Convert layer folders to strings
    let layers: Vec<String> = layer_folders
        .iter()
        .filter_map(|p| p.to_str().map(String::from))
        .collect();

    let mut windows_config = json!({
        "layerFolders": layers,
        "resources": {
            "memory": {
                "limit": (config.memory_mb as u64) * 1024 * 1024
            },
            "cpu": {
                "count": config.cpus as u64
            }
        },
        "network": {
            "endpointList": []
        }
    });

    // Add Hyper-V configuration if using Hyper-V isolation
    if isolation == WindowsIsolation::HyperV {
        windows_config["hyperv"] = json!({});
    }

    json!({
        "ociVersion": "1.0.2",
        "process": {
            "terminal": false,
            "user": {
                "username": "ContainerUser"
            },
            "args": ["cmd.exe", "/c", "echo ready"],
            "env": env,
            "cwd": config.workdir.replace('/', "\\"),
        },
        "root": {
            "path": rootfs_path.to_str().unwrap_or("rootfs"),
        },
        "hostname": config.name.chars().take(15).collect::<String>(),
        "windows": windows_config
    })
}

/// Default environment variables for Windows containers
fn default_windows_env() -> Vec<String> {
    vec![
        "PATH=C:\\Windows\\system32;C:\\Windows;C:\\Windows\\System32\\Wbem;C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\".to_string(),
        "PATHEXT=.COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC".to_string(),
        "COMPUTERNAME=SANDBOX".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_windows_isolation_default() {
        assert_eq!(WindowsIsolation::default(), WindowsIsolation::Process);
    }

    #[test]
    fn test_generate_windows_oci_config() {
        let config = SandboxConfig::builder()
            .name("test-sandbox")
            .image("mcr.microsoft.com/windows/nanoserver:ltsc2022")
            .cpus(2)
            .memory_mb(2048)
            .build();

        let oci_config = generate_windows_oci_config(
            &config,
            Path::new("C:\\containers\\test\\rootfs"),
            &[PathBuf::from("C:\\containers\\layers\\base")],
            WindowsIsolation::Process,
        );

        assert_eq!(oci_config["ociVersion"], "1.0.2");
        assert!(
            oci_config["windows"]["resources"]["memory"]["limit"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert!(oci_config["windows"]["hyperv"].is_null());
    }

    #[test]
    fn test_generate_windows_oci_config_hyperv() {
        let config = SandboxConfig::builder()
            .name("test-sandbox")
            .image("mcr.microsoft.com/windows/nanoserver:ltsc2022")
            .cpus(2)
            .memory_mb(2048)
            .build();

        let oci_config = generate_windows_oci_config(
            &config,
            Path::new("C:\\containers\\test\\rootfs"),
            &[],
            WindowsIsolation::HyperV,
        );

        // Hyper-V config should be present
        assert!(oci_config["windows"]["hyperv"].is_object());
    }

    #[test]
    fn test_default_windows_env() {
        let env = default_windows_env();
        assert!(env.iter().any(|e| e.starts_with("PATH=")));
        assert!(env.iter().any(|e| e.starts_with("PATHEXT=")));
    }
}
