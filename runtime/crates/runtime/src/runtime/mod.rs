//! Runtime backend abstraction for sandbox execution
//!
//! This module provides platform-specific runtime backends:
//!
//! | Platform | Runtime | Hypervisor |
//! |----------|---------|------------|
//! | Linux | libkrun FFI | KVM |
//! | macOS | libkrun FFI | HVF (Hypervisor.framework) |
//!
//! The runtime is a pure VM engine. Gateway communication, SSH setup, and
//! orchestration logic belong in the sandbox layer.

pub mod validation;

pub use validation::{
    validate_runtime_prerequisites, validate_runtime_prerequisites_detailed, ValidationResult,
};

// libkrun FFI bindings — libkrun-sys rlib on all platforms
mod ffi;

// libkrun runtime backend
mod libkrun;

// gvproxy networking (Unix only — uses Unix sockets)
pub(crate) mod gvproxy;

pub use self::libkrun::LibkrunRuntime;

pub use self::libkrun::handle_boot_vm_subprocess;

/// Check if gvproxy is available on this system.
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

use crate::config::SandboxConfig;
use crate::error::Result;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use crate::error::Error;
use std::path::Path;
use tracing::info;

/// Runtime backend enum — pure VM engine.
///
/// All platforms use the same backend:
/// - Linux/macOS: `Libkrun` (direct FFI via libkrun C API)
pub enum RuntimeBackend {
    Libkrun(LibkrunRuntime),
}

impl RuntimeBackend {
    pub fn name(&self) -> &str {
        match self {
            RuntimeBackend::Libkrun(_) => "libkrun",
        }
    }

    pub async fn create(
        &self,
        id: &str,
        config: &SandboxConfig,
        bundle_path: Option<&Path>,
    ) -> Result<()> {
        match self {
            RuntimeBackend::Libkrun(r) => r.create(id, config, bundle_path).await,
        }
    }

    pub async fn start(&self, id: &str) -> Result<()> {
        match self {
            RuntimeBackend::Libkrun(r) => r.start(id).await,
        }
    }

    /// Dynamically forward a guest port to the same host port via gvproxy.
    pub fn expose_port(&self, id: &str, port: u16) -> std::result::Result<(), String> {
        match self {
            RuntimeBackend::Libkrun(r) => r.expose_port(id, port),
        }
    }

    /// Get the guest IP address for a sandbox.
    pub fn guest_ip(&self, id: &str) -> Option<String> {
        match self {
            RuntimeBackend::Libkrun(r) => r.guest_ip(id),
        }
    }

    pub fn is_vm_running(&self, id: &str) -> bool {
        match self {
            RuntimeBackend::Libkrun(r) => r.is_vm_running(id),
        }
    }

    pub async fn stop(&self, id: &str) -> Result<()> {
        match self {
            RuntimeBackend::Libkrun(r) => r.stop(id).await,
        }
    }

    pub async fn destroy(&self, id: &str) -> Result<()> {
        match self {
            RuntimeBackend::Libkrun(r) => r.destroy(id).await,
        }
    }
}

/// Detect and create the runtime for the current platform
pub async fn detect_runtime() -> Result<RuntimeBackend> {
    info!("Initializing libkrun FFI runtime - direct VM management");
    let runtime = LibkrunRuntime::new().await?;
    info!("Using libkrun FFI runtime");
    Ok(RuntimeBackend::Libkrun(runtime))
}

/// Runtime wrapper — pure VM engine.
///
/// The runtime manages VM lifecycle (create, start, stop, exec).
/// Gateway communication, SSH, and orchestration belong in the sandbox layer.
pub struct Runtime {
    backend: RuntimeBackend,
}

impl Runtime {
    pub async fn new() -> Result<Self> {
        let backend = detect_runtime().await?;
        Ok(Self { backend })
    }

    pub fn name(&self) -> &str {
        self.backend.name()
    }

    pub fn binary_path(&self) -> &str {
        self.backend.name()
    }

    pub async fn create(
        &self,
        id: &str,
        config: &SandboxConfig,
        bundle_path: Option<&Path>,
    ) -> Result<()> {
        self.backend.create(id, config, bundle_path).await
    }

    pub async fn start(&self, id: &str) -> Result<()> {
        self.backend.start(id).await
    }

    /// Dynamically forward a guest port to the same host port via gvproxy.
    pub fn expose_port(&self, id: &str, port: u16) -> std::result::Result<(), String> {
        self.backend.expose_port(id, port)
    }

    /// Get the guest IP address for a sandbox.
    pub fn guest_ip(&self, id: &str) -> Option<String> {
        self.backend.guest_ip(id)
    }

    /// Check if the VM process is still running.
    pub fn is_vm_running(&self, id: &str) -> bool {
        self.backend.is_vm_running(id)
    }

    /// Stop a VM (force kill).
    pub async fn kill(&self, id: &str) -> Result<()> {
        self.backend.stop(id).await
    }

    /// Delete a VM and clean up resources.
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
                println!("Runtime not available: {}", e);
            }
        }
    }
}
