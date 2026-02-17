//! Runtime backend abstraction for sandbox execution
//!
//! This module provides platform-specific runtime backends:
//!
//! | Platform | Runtime | Hypervisor |
//! |----------|---------|------------|
//! | Linux | libkrun FFI | KVM |
//! | macOS | libkrun FFI | HVF (Hypervisor.framework) |
//! | Windows | containerd + runhcs shim | HCS (Hyper-V/Process isolation) |
//!
//! On macOS and Linux, the libkrun FFI backend calls libkrun's C API directly
//! for VM management. Image pulling and rootfs preparation are handled by the
//! pure-Rust ImageManager component.

pub mod validation;

// libkrun direct FFI backend (macOS + Linux)
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod ffi;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) mod gvproxy;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod libkrun;

// Windows containerd runtime (primary Windows runtime)
#[cfg(target_os = "windows")]
pub mod containerd_windows;

// Legacy Windows Container runtime (deprecated, kept for reference)
#[cfg(target_os = "windows")]
pub mod windows;

// Legacy Windows runhcs setup (deprecated, kept for reference)
#[cfg(target_os = "windows")]
pub mod runhcs_setup;

pub use validation::validate_runtime_prerequisites;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use self::libkrun::LibkrunRuntime;

/// Check if gvproxy is available on this system.
///
/// Used by the sandbox orchestrator to decide DNS configuration (gvproxy gateway
/// vs host DNS). On non-Linux/macOS platforms, always returns false.
pub fn gvproxy_available() -> bool {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        gvproxy::GvproxyManager::is_available()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

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
/// Each platform has a single runtime backend:
/// - Linux/macOS: `Libkrun` (direct FFI via libkrun C API)
/// - Windows: `WindowsContainerd` (containerd + runhcs shim)
pub enum RuntimeBackend {
    /// Direct libkrun FFI Runtime - macOS and Linux
    /// Uses TSI networking, no CLI binary dependency, pure-Rust image handling
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Libkrun(LibkrunRuntime),

    /// Windows containerd Runtime - Windows only (primary)
    /// Uses containerd with containerd-shim-runhcs-v1 for proper layer/snapshot management
    #[cfg(target_os = "windows")]
    WindowsContainerd(ContainerdWindowsRuntime),
}

impl RuntimeBackend {
    /// Get the runtime name
    pub fn name(&self) -> &str {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(_) => "libkrun",
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
    /// - Libkrun: `false` -- uses ImageManager (pure Rust) via Sandbox orchestrator
    /// - Windows containerd: `true` -- containerd handles image pull + snapshots
    pub fn handles_image_pull(&self) -> bool {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.handles_image_pull(),
            }
        }

        #[cfg(target_os = "windows")]
        {
            match self {
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
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.create(id, config, bundle_path).await,
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
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.start(id).await,
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
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.exec(id, command, args, workdir, env).await,
            }
        }

        #[cfg(target_os = "windows")]
        {
            match self {
                RuntimeBackend::WindowsContainerd(r) => {
                    r.exec(id, command, args, workdir, env).await
                }
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
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => {
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

    /// Send a structured agent message via the gateway's /api/v1/message endpoint.
    ///
    /// Only available when the sandbox is in persistent (gateway) mode.
    /// On Windows, this always returns an error (no gateway support yet).
    pub async fn send_message<F>(
        &self,
        id: &str,
        message: &str,
        agent: &str,
        model: &str,
        env: &HashMap<String, String>,
        on_output: F,
    ) -> Result<i32>
    where
        F: Fn(&str, bool) + Send + Sync,
    {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => {
                    r.send_message(id, message, agent, model, env, on_output)
                        .await
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = (id, message, agent, model, env, on_output);
            Err(crate::error::Error::ExecFailed(
                "Agent gateway not supported on Windows yet".to_string(),
            ))
        }
    }

    /// Check if the sandbox is in persistent (gateway) mode.
    pub fn is_persistent(&self, id: &str) -> bool {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.is_persistent(id),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = id;
            false
        }
    }

    /// Stop the sandbox/VM
    pub async fn stop(&self, id: &str) -> Result<()> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.stop(id).await,
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
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.destroy(id).await,
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
/// Platform runtime selection:
/// - Linux  -> LibkrunRuntime (direct FFI, requires libkrun.so)
/// - macOS  -> LibkrunRuntime (direct FFI, requires libkrun.dylib)
/// - Windows -> ContainerdWindowsRuntime (containerd + runhcs shim)
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
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        info!("Initializing libkrun FFI runtime - direct VM management");
        let runtime = LibkrunRuntime::new().await?;
        info!("Using libkrun FFI runtime");
        Ok(RuntimeBackend::Libkrun(runtime))
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

    /// Send a structured agent message via the gateway.
    ///
    /// Only works in persistent (gateway) mode. The gateway handles agent CLI
    /// spawning, session continuity, and streams output as SSE events.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_message<F>(
        &self,
        id: &str,
        message: &str,
        agent: &str,
        model: &str,
        env: &HashMap<String, String>,
        on_output: F,
    ) -> Result<i32>
    where
        F: Fn(&str, bool) + Send + Sync,
    {
        self.backend
            .send_message(id, message, agent, model, env, on_output)
            .await
    }

    /// Check if the sandbox is in persistent (gateway) mode.
    pub fn is_persistent(&self, id: &str) -> bool {
        self.backend.is_persistent(id)
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
