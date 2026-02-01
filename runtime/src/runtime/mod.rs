//! Runtime backend abstraction for sandbox execution
//!
//! Supports multiple runtime backends:
//! - `OciRuntime` - crun/krun (OCI runtimes with libkrun) for Linux
//! - `KrunVmRuntime` - krunvm for macOS Apple Silicon

mod krunvm;
mod oci;

pub use krunvm::KrunVmRuntime;
pub use oci::OciRuntime;

use crate::config::SandboxConfig;
use crate::error::{Error, Result};
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
/// Uses enum dispatch instead of trait objects to avoid dyn compatibility issues
/// with generic methods like exec_stream.
pub enum RuntimeBackend {
    /// OCI Runtime (crun/krun)
    Oci(OciRuntime),
    /// krunvm Runtime (macOS)
    KrunVm(KrunVmRuntime),
}

impl RuntimeBackend {
    /// Get the runtime name
    pub fn name(&self) -> &str {
        match self {
            RuntimeBackend::Oci(r) => r.name(),
            RuntimeBackend::KrunVm(_) => "krunvm",
        }
    }

    /// Check if this runtime handles image pulling internally
    pub fn handles_image_pull(&self) -> bool {
        match self {
            RuntimeBackend::Oci(_) => false,
            RuntimeBackend::KrunVm(_) => true,
        }
    }

    /// Create a sandbox/VM
    pub async fn create(
        &self,
        id: &str,
        config: &SandboxConfig,
        bundle_path: Option<&Path>,
    ) -> Result<()> {
        match self {
            RuntimeBackend::Oci(r) => r.create(id, config, bundle_path).await,
            RuntimeBackend::KrunVm(r) => r.create(id, config, bundle_path).await,
        }
    }

    /// Start the sandbox/VM
    pub async fn start(&self, id: &str) -> Result<()> {
        match self {
            RuntimeBackend::Oci(r) => r.start(id).await,
            RuntimeBackend::KrunVm(r) => r.start(id).await,
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
        match self {
            RuntimeBackend::Oci(r) => r.exec(id, command, args, workdir, env).await,
            RuntimeBackend::KrunVm(r) => r.exec(id, command, args, workdir, env).await,
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
        match self {
            RuntimeBackend::Oci(r) => {
                r.exec_stream(id, command, args, workdir, env, on_output)
                    .await
            }
            RuntimeBackend::KrunVm(r) => {
                r.exec_stream(id, command, args, workdir, env, on_output)
                    .await
            }
        }
    }

    /// Stop the sandbox/VM
    pub async fn stop(&self, id: &str) -> Result<()> {
        match self {
            RuntimeBackend::Oci(r) => r.stop(id).await,
            RuntimeBackend::KrunVm(r) => r.stop(id).await,
        }
    }

    /// Destroy/delete the sandbox/VM
    pub async fn destroy(&self, id: &str) -> Result<()> {
        match self {
            RuntimeBackend::Oci(r) => r.destroy(id).await,
            RuntimeBackend::KrunVm(r) => r.destroy(id).await,
        }
    }
}

/// Detect and create the best available runtime for the current platform
pub async fn detect_runtime() -> Result<RuntimeBackend> {
    // Try OCI runtime first (Linux with crun/krun)
    if OciRuntime::is_available().await {
        info!("Using OCI runtime (crun/krun)");
        return Ok(RuntimeBackend::Oci(OciRuntime::new().await?));
    }

    // Try krunvm (macOS Apple Silicon)
    if KrunVmRuntime::is_available().await {
        info!("Using krunvm runtime");
        return Ok(RuntimeBackend::KrunVm(KrunVmRuntime::new().await?));
    }

    Err(Error::RuntimeNotAvailable(
        "No runtime available. Install crun/krun (Linux) or krunvm (macOS).".to_string(),
    ))
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
    pub async fn exec(
        &self,
        id: &str,
        command: &str,
        args: &[&str],
    ) -> Result<ExecOutput> {
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
        match Runtime::new().await {
            Ok(runtime) => {
                println!("Runtime detected: {}", runtime.name());
                assert!(!runtime.name().is_empty());
            }
            Err(e) => {
                eprintln!("Runtime not available for testing: {}", e);
            }
        }
    }
}
