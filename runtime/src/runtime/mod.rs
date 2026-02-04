//! Runtime backend abstraction for sandbox execution
//!
//! This module provides platform-specific runtime backends:
//!
//! | Platform | Runtime | Hypervisor |
//! |----------|---------|------------|
//! | Linux | crun/krun (OCI) | KVM |
//! | macOS | krunvm | HVF (Hypervisor.framework) |
//! | Windows | containerd + runhcs shim | HCS (Hyper-V/Process isolation) |
//!
//! Each platform has exactly one runtime. If the runtime prerequisites
//! are not met, an error is returned with installation instructions.

mod krunvm;
mod oci;
pub mod validation;

// Windows containerd runtime (primary Windows runtime)
#[cfg(target_os = "windows")]
pub mod containerd_windows;

// Legacy Windows Container runtime (deprecated, kept for reference)
#[cfg(target_os = "windows")]
pub mod windows;

// Legacy Windows runhcs setup (deprecated, kept for reference)
#[cfg(target_os = "windows")]
pub mod runhcs_setup;

pub use krunvm::KrunVmRuntime;
pub use oci::OciRuntime;
pub use validation::validate_runtime_prerequisites;

#[cfg(target_os = "windows")]
pub use containerd_windows::{ContainerdWindowsRuntime, WindowsContainerdIsolation};

// Legacy exports (deprecated, kept for backward compatibility)
#[cfg(target_os = "windows")]
#[allow(deprecated)]
pub use windows::{WindowsContainerRuntime, WindowsIsolation};

use crate::config::SandboxConfig;
use crate::error::Result;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
use crate::error::Error;
use std::collections::HashMap;
use std::path::Path;
use tracing::info;

/// Output from command execution
#[derive(Debug, Clone)]
pub struct ExecOutput {
    /// Exit code of the command
    pub exit_code: i32,
    /// Standard output
    pub stdout: String,
    /// Standard error
    pub stderr: String,
}

/// Runtime backend enum
///
/// Each platform has exactly one runtime backend:
/// - Linux: `Oci` (crun/krun)
/// - macOS: `KrunVm` (krunvm)
/// - Windows: `WindowsContainerd` (containerd + runhcs shim)
pub enum RuntimeBackend {
    /// OCI Runtime (crun/krun) - Linux only
    #[cfg(target_os = "linux")]
    Oci(OciRuntime),

    /// krunvm Runtime - macOS Apple Silicon only
    #[cfg(target_os = "macos")]
    KrunVm(KrunVmRuntime),

    /// Windows containerd Runtime - Windows only (primary)
    /// Uses containerd with containerd-shim-runhcs-v1 for proper layer/snapshot management
    #[cfg(target_os = "windows")]
    WindowsContainerd(ContainerdWindowsRuntime),
}

impl RuntimeBackend {
    /// Get the runtime name
    pub fn name(&self) -> &str {
        #[cfg(target_os = "linux")]
        {
            match self {
                RuntimeBackend::Oci(r) => r.name(),
            }
        }

        #[cfg(target_os = "macos")]
        {
            match self {
                RuntimeBackend::KrunVm(_) => "krunvm",
            }
        }

        #[cfg(target_os = "windows")]
        {
            match self {
                RuntimeBackend::WindowsContainerd(r) => r.name(),
            }
        }
    }

    /// Check if this runtime handles image pulling internally
    ///
    /// Windows containerd runtime handles image pull via containerd,
    /// so nanosandbox does not need to pull images separately.
    pub fn handles_image_pull(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            match self {
                RuntimeBackend::Oci(_) => false,
            }
        }

        #[cfg(target_os = "macos")]
        {
            match self {
                RuntimeBackend::KrunVm(_) => true,
            }
        }

        #[cfg(target_os = "windows")]
        {
            match self {
                // containerd handles image pull + snapshot management
                RuntimeBackend::WindowsContainerd(_) => true,
            }
        }
    }

    /// Create a sandbox/VM
    pub async fn create(
        &self,
        id: &str,
        config: &SandboxConfig,
        bundle_path: Option<&Path>,
    ) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            match self {
                RuntimeBackend::Oci(r) => r.create(id, config, bundle_path).await,
            }
        }

        #[cfg(target_os = "macos")]
        {
            match self {
                RuntimeBackend::KrunVm(r) => r.create(id, config, bundle_path).await,
            }
        }

        #[cfg(target_os = "windows")]
        {
            match self {
                RuntimeBackend::WindowsContainerd(r) => r.create(id, config, bundle_path).await,
            }
        }
    }

    /// Start the sandbox/VM
    pub async fn start(&self, id: &str) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            match self {
                RuntimeBackend::Oci(r) => r.start(id).await,
            }
        }

        #[cfg(target_os = "macos")]
        {
            match self {
                RuntimeBackend::KrunVm(r) => r.start(id).await,
            }
        }

        #[cfg(target_os = "windows")]
        {
            match self {
                RuntimeBackend::WindowsContainerd(r) => r.start(id).await,
            }
        }
    }

    /// Execute a command
    pub async fn exec(
        &self,
        id: &str,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
    ) -> Result<ExecOutput> {
        #[cfg(target_os = "linux")]
        {
            match self {
                RuntimeBackend::Oci(r) => r.exec(id, command, args, workdir, env).await,
            }
        }

        #[cfg(target_os = "macos")]
        {
            match self {
                RuntimeBackend::KrunVm(r) => r.exec(id, command, args, workdir, env).await,
            }
        }

        #[cfg(target_os = "windows")]
        {
            match self {
                RuntimeBackend::WindowsContainerd(r) => r.exec(id, command, args, workdir, env).await,
            }
        }
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
        #[cfg(target_os = "linux")]
        {
            match self {
                RuntimeBackend::Oci(r) => {
                    r.exec_stream(id, command, args, workdir, env, on_output)
                        .await
                }
            }
        }

        #[cfg(target_os = "macos")]
        {
            match self {
                RuntimeBackend::KrunVm(r) => {
                    r.exec_stream(id, command, args, workdir, env, on_output)
                        .await
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            match self {
                RuntimeBackend::WindowsContainerd(r) => {
                    r.exec_stream(id, command, args, workdir, env, on_output)
                        .await
                }
            }
        }
    }

    /// Stop the sandbox/VM
    pub async fn stop(&self, id: &str) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            match self {
                RuntimeBackend::Oci(r) => r.stop(id).await,
            }
        }

        #[cfg(target_os = "macos")]
        {
            match self {
                RuntimeBackend::KrunVm(r) => r.stop(id).await,
            }
        }

        #[cfg(target_os = "windows")]
        {
            match self {
                RuntimeBackend::WindowsContainerd(r) => r.stop(id).await,
            }
        }
    }

    /// Destroy/delete the sandbox/VM
    pub async fn destroy(&self, id: &str) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            match self {
                RuntimeBackend::Oci(r) => r.destroy(id).await,
            }
        }

        #[cfg(target_os = "macos")]
        {
            match self {
                RuntimeBackend::KrunVm(r) => r.destroy(id).await,
            }
        }

        #[cfg(target_os = "windows")]
        {
            match self {
                RuntimeBackend::WindowsContainerd(r) => r.destroy(id).await,
            }
        }
    }
}

/// Detect and create the runtime for the current platform
///
/// This function:
/// 1. Validates all runtime prerequisites
/// 2. Creates the platform-specific runtime
///
/// Each platform has exactly one runtime:
/// - Linux → OciRuntime (crun/krun)
/// - macOS → KrunVmRuntime (krunvm)
/// - Windows → ContainerdWindowsRuntime (containerd + runhcs shim)
///
/// # Errors
///
/// Returns an error if:
/// - The platform is not supported
/// - Runtime prerequisites are not met (with installation instructions)
/// - Runtime initialization fails
pub async fn detect_runtime() -> Result<RuntimeBackend> {
    // First, validate prerequisites
    validate_runtime_prerequisites().await?;

    // Then create the platform-specific runtime
    #[cfg(target_os = "linux")]
    {
        info!("Initializing OCI runtime (Linux)");
        let runtime = OciRuntime::new().await?;
        info!("Using OCI runtime: {}", runtime.name());
        Ok(RuntimeBackend::Oci(runtime))
    }

    #[cfg(target_os = "macos")]
    {
        info!("Initializing krunvm runtime (macOS)");
        let runtime = KrunVmRuntime::new().await?;
        info!("Using krunvm runtime");
        Ok(RuntimeBackend::KrunVm(runtime))
    }

    #[cfg(target_os = "windows")]
    {
        info!("Initializing Windows containerd runtime");
        let runtime = ContainerdWindowsRuntime::new().await?;
        info!("Using Windows containerd runtime (containerd + runhcs shim)");
        Ok(RuntimeBackend::WindowsContainerd(runtime))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Err(Error::UnsupportedPlatform {
            platform: std::env::consts::OS.to_string(),
        })
    }
}

/// Runtime wrapper for backward compatibility
pub struct Runtime {
    backend: RuntimeBackend,
}

impl Runtime {
    /// Create a new runtime (auto-detects best backend)
    pub async fn new() -> Result<Self> {
        let backend = detect_runtime().await?;
        Ok(Self { backend })
    }

    /// Get the runtime name
    pub fn name(&self) -> &str {
        self.backend.name()
    }

    /// Get the runtime binary path (for compatibility)
    pub fn binary_path(&self) -> &str {
        self.backend.name()
    }

    /// Check if this runtime handles image pulling
    pub fn handles_image_pull(&self) -> bool {
        self.backend.handles_image_pull()
    }

    /// Create a container/VM
    pub async fn create(
        &self,
        id: &str,
        config: &SandboxConfig,
        bundle_path: Option<&Path>,
    ) -> Result<()> {
        self.backend.create(id, config, bundle_path).await
    }

    /// Start a container/VM
    pub async fn start(&self, id: &str) -> Result<()> {
        self.backend.start(id).await
    }

    /// Execute a command
    pub async fn exec(&self, id: &str, command: &str, args: &[&str]) -> Result<ExecOutput> {
        self.backend
            .exec(id, command, args, None, &HashMap::new())
            .await
    }

    /// Execute a command with options
    pub async fn exec_with_options(
        &self,
        id: &str,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
        _user: Option<&str>,
    ) -> Result<ExecOutput> {
        self.backend.exec(id, command, args, workdir, env).await
    }

    /// Execute with streaming output
    #[allow(clippy::too_many_arguments)]
    pub async fn exec_stream<F>(
        &self,
        id: &str,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
        _user: Option<&str>,
        on_output: F,
    ) -> Result<i32>
    where
        F: Fn(&str, bool) + Send + Sync,
    {
        self.backend
            .exec_stream(id, command, args, workdir, env, on_output)
            .await
    }

    /// Stop a container/VM
    pub async fn kill(&self, id: &str) -> Result<()> {
        self.backend.stop(id).await
    }

    /// Delete a container/VM
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.backend.destroy(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_runtime_creation() {
        // This test will pass on supported platforms with prerequisites met,
        // and provide helpful error messages on unsupported platforms
        match Runtime::new().await {
            Ok(runtime) => {
                println!("Runtime detected: {}", runtime.name());
                assert!(!runtime.name().is_empty());
            }
            Err(e) => {
                // Expected on systems without runtime prerequisites
                eprintln!("Runtime not available: {}", e);
            }
        }
    }
}
