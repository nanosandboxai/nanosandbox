//! Runtime backend abstraction for sandbox execution
//!
//! This module provides platform-specific runtime backends:
//!
//! | Platform | Runtime | Hypervisor |
//! |----------|---------|------------|
//! | Linux | libkrun FFI | KVM |
//! | macOS | libkrun FFI | HVF (Hypervisor.framework) |
//! | Windows | libkrun FFI (WHPX) | WHPX (Windows Hypervisor Platform) |
//!
//! On all platforms, the libkrun FFI backend calls libkrun's C API directly
//! for VM management. Image pulling and rootfs preparation are handled by the
//! pure-Rust ImageManager component.

pub mod validation;

pub use validation::{
    validate_runtime_prerequisites, validate_runtime_prerequisites_detailed, ValidationResult,
};

// libkrun FFI bindings (all platforms)
mod ffi;

// libkrun runtime backend — Unix uses fork-based subprocess, Windows uses thread-based
#[cfg(not(target_os = "windows"))]
mod libkrun;
#[cfg(target_os = "windows")]
#[path = "libkrun_windows.rs"]
mod libkrun;

// gvproxy networking (Unix only — uses Unix sockets)
#[cfg(not(target_os = "windows"))]
pub(crate) mod gvproxy;
// gvproxy stub for Windows (always returns "not available", uses TSI fallback)
#[cfg(target_os = "windows")]
#[path = "gvproxy_stub.rs"]
pub(crate) mod gvproxy;

pub use self::libkrun::LibkrunRuntime;

pub use self::libkrun::handle_boot_vm_subprocess;

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

use crate::config::SandboxConfig;
use crate::config::{McpServerConfig, ResolvedAgentConfig, SkillDef};
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
/// All platforms use the same backend:
/// - Linux/macOS/Windows: `Libkrun` (direct FFI via libkrun C API)
pub enum RuntimeBackend {
    /// Direct libkrun FFI Runtime - all platforms
    /// Uses TSI networking on Windows, gvproxy on macOS/Linux
    Libkrun(LibkrunRuntime),
}

impl RuntimeBackend {
    /// Get the runtime name
    pub fn name(&self) -> &str {
        match self {
            RuntimeBackend::Libkrun(_) => "libkrun",
        }
    }

    /// Check if this runtime handles image pulling internally
    ///
    /// - Libkrun: `false` -- uses ImageManager (pure Rust) via Sandbox orchestrator
    /// - Windows containerd: `true` -- containerd handles image pull + snapshots
    pub fn handles_image_pull(&self) -> bool {
        match self {
            RuntimeBackend::Libkrun(r) => r.handles_image_pull(),
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
            RuntimeBackend::Libkrun(r) => r.create(id, config, bundle_path).await,
        }
    }

    /// Start the sandbox/VM
    pub async fn start(&self, id: &str) -> Result<()> {
        match self {
            RuntimeBackend::Libkrun(r) => r.start(id).await,
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
            RuntimeBackend::Libkrun(r) => r.exec(id, command, args, workdir, env).await,
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
            RuntimeBackend::Libkrun(r) => {
                r.exec_stream(id, command, args, workdir, env, on_output)
                    .await
            }
        }
    }

    /// Send a structured agent message via the gateway's /api/v1/message endpoint.
    ///
    /// Only available when the sandbox is in persistent (gateway) mode.
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
        match self {
            RuntimeBackend::Libkrun(r) => {
                r.send_message(id, message, agent, model, env, on_output)
                    .await
            }
        }
    }

    /// Check if the sandbox is in persistent (gateway) mode.
    pub fn is_persistent(&self, id: &str) -> bool {
        match self {
            RuntimeBackend::Libkrun(r) => r.is_persistent(id),
        }
    }

    /// Get the SSH host port for a sandbox (if available).
    pub fn ssh_port(&self, id: &str) -> Option<u16> {
        match self {
            RuntimeBackend::Libkrun(r) => r.ssh_port(id),
        }
    }

    /// Dynamically forward a guest port to the same host port via gvproxy.
    pub fn expose_port(&self, id: &str, port: u16) -> std::result::Result<(), String> {
        match self {
            RuntimeBackend::Libkrun(r) => r.expose_port(id, port),
        }
    }

    /// Get the SSH private key path for a sandbox (if available).
    pub fn ssh_key_path(&self, id: &str) -> Option<std::path::PathBuf> {
        match self {
            RuntimeBackend::Libkrun(r) => r.ssh_key_path(id),
        }
    }

    /// Build a ready-to-use SSH command string for connecting to a sandbox.
    pub fn ssh_command(&self, id: &str) -> Option<String> {
        match self {
            RuntimeBackend::Libkrun(r) => r.ssh_command(id),
        }
    }

    /// Push MCP server configurations to the gateway (bulk, on start).
    pub fn push_mcp_config(
        &self,
        id: &str,
        servers: &HashMap<String, McpServerConfig>,
    ) -> Result<()> {
        match self {
            RuntimeBackend::Libkrun(r) => r.push_mcp_config(id, servers),
        }
    }

    /// Add or update an MCP server.
    pub fn add_mcp_server(&self, id: &str, name: &str, config: &McpServerConfig) -> Result<()> {
        match self {
            RuntimeBackend::Libkrun(r) => r.add_mcp_server(id, name, config),
        }
    }

    /// Remove an MCP server.
    pub fn remove_mcp_server(&self, id: &str, name: &str) -> Result<()> {
        match self {
            RuntimeBackend::Libkrun(r) => r.remove_mcp_server(id, name),
        }
    }

    /// List all MCP servers.
    pub fn list_mcp_servers(&self, id: &str) -> Result<HashMap<String, McpServerConfig>> {
        match self {
            RuntimeBackend::Libkrun(r) => r.list_mcp_servers(id),
        }
    }

    /// Enable an MCP server.
    pub fn enable_mcp_server(&self, id: &str, name: &str) -> Result<()> {
        match self {
            RuntimeBackend::Libkrun(r) => r.enable_mcp_server(id, name),
        }
    }

    /// Disable an MCP server.
    pub fn disable_mcp_server(&self, id: &str, name: &str) -> Result<()> {
        match self {
            RuntimeBackend::Libkrun(r) => r.disable_mcp_server(id, name),
        }
    }

    /// Add a skill to the running sandbox.
    pub fn add_skill(&self, id: &str, skill: &SkillDef) -> Result<()> {
        match self {
            RuntimeBackend::Libkrun(r) => r.add_skill(id, skill),
        }
    }

    /// Remove a skill from the running sandbox.
    pub fn remove_skill(&self, id: &str, name: &str) -> Result<()> {
        match self {
            RuntimeBackend::Libkrun(r) => r.remove_skill(id, name),
        }
    }

    /// List all skills in the running sandbox.
    pub fn list_skills(&self, id: &str) -> Result<HashMap<String, SkillDef>> {
        match self {
            RuntimeBackend::Libkrun(r) => r.list_skills(id),
        }
    }

    /// Bootstrap agent definition + skills + MCPs in one call.
    pub fn bootstrap_agent(&self, id: &str, config: &ResolvedAgentConfig) -> Result<()> {
        match self {
            RuntimeBackend::Libkrun(r) => r.bootstrap_agent(id, config),
        }
    }

    /// Set the agent definition (name + prompt).
    pub fn set_agent(&self, id: &str, name: &str, prompt: &str) -> Result<()> {
        match self {
            RuntimeBackend::Libkrun(r) => r.set_agent(id, name, prompt),
        }
    }

    /// Restart the agent process in the running sandbox.
    pub fn restart_agent(&self, id: &str, reason: &str) -> Result<serde_json::Value> {
        match self {
            RuntimeBackend::Libkrun(r) => r.restart_agent(id, reason),
        }
    }

    /// Stop the sandbox/VM
    pub async fn stop(&self, id: &str) -> Result<()> {
        match self {
            RuntimeBackend::Libkrun(r) => r.stop(id).await,
        }
    }

    /// Destroy/delete the sandbox/VM
    pub async fn destroy(&self, id: &str) -> Result<()> {
        match self {
            RuntimeBackend::Libkrun(r) => r.destroy(id).await,
        }
    }
}

/// Detect and create the runtime for the current platform
///
/// Platform runtime selection:
/// - Linux   -> LibkrunRuntime (direct FFI, requires libkrun.so)
/// - macOS   -> LibkrunRuntime (direct FFI, requires libkrun.dylib)
/// - Windows -> LibkrunRuntime (direct FFI via WHPX, requires krun.dll)
///
/// # Errors
///
/// Returns an error if:
/// - The platform is not supported
/// - Runtime prerequisites are not met (with installation instructions)
/// - Runtime initialization fails
pub async fn detect_runtime() -> Result<RuntimeBackend> {
    // All platforms use libkrun FFI runtime
    info!("Initializing libkrun FFI runtime - direct VM management");
    let runtime = LibkrunRuntime::new().await?;
    info!("Using libkrun FFI runtime");
    Ok(RuntimeBackend::Libkrun(runtime))
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

    /// Get the SSH host port for a sandbox (if available).
    pub fn ssh_port(&self, id: &str) -> Option<u16> {
        self.backend.ssh_port(id)
    }

    /// Dynamically forward a guest port to the same host port via gvproxy.
    pub fn expose_port(&self, id: &str, port: u16) -> std::result::Result<(), String> {
        self.backend.expose_port(id, port)
    }

    /// Get the SSH private key path for a sandbox (if available).
    pub fn ssh_key_path(&self, id: &str) -> Option<std::path::PathBuf> {
        self.backend.ssh_key_path(id)
    }

    /// Build a ready-to-use SSH command string for connecting to a sandbox.
    pub fn ssh_command(&self, id: &str) -> Option<String> {
        self.backend.ssh_command(id)
    }

    /// Stop a container/VM
    pub async fn kill(&self, id: &str) -> Result<()> {
        self.backend.stop(id).await
    }

    /// Delete a container/VM
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.backend.destroy(id).await
    }

    /// Push MCP server configs to the gateway (bulk, on start).
    pub fn push_mcp_config(
        &self,
        id: &str,
        servers: &HashMap<String, McpServerConfig>,
    ) -> Result<()> {
        self.backend.push_mcp_config(id, servers)
    }

    /// Add or update an MCP server in the running sandbox.
    pub fn add_mcp_server(&self, id: &str, name: &str, config: &McpServerConfig) -> Result<()> {
        self.backend.add_mcp_server(id, name, config)
    }

    /// Remove an MCP server from the running sandbox.
    pub fn remove_mcp_server(&self, id: &str, name: &str) -> Result<()> {
        self.backend.remove_mcp_server(id, name)
    }

    /// List all MCP servers in the running sandbox.
    pub fn list_mcp_servers(&self, id: &str) -> Result<HashMap<String, McpServerConfig>> {
        self.backend.list_mcp_servers(id)
    }

    /// Enable an MCP server in the running sandbox.
    pub fn enable_mcp_server(&self, id: &str, name: &str) -> Result<()> {
        self.backend.enable_mcp_server(id, name)
    }

    /// Disable an MCP server in the running sandbox.
    pub fn disable_mcp_server(&self, id: &str, name: &str) -> Result<()> {
        self.backend.disable_mcp_server(id, name)
    }

    /// Add a skill to the running sandbox.
    pub fn add_skill(&self, id: &str, skill: &SkillDef) -> Result<()> {
        self.backend.add_skill(id, skill)
    }

    /// Remove a skill from the running sandbox.
    pub fn remove_skill(&self, id: &str, name: &str) -> Result<()> {
        self.backend.remove_skill(id, name)
    }

    /// List all skills in the running sandbox.
    pub fn list_skills(&self, id: &str) -> Result<HashMap<String, SkillDef>> {
        self.backend.list_skills(id)
    }

    /// Bootstrap agent definition + skills + MCPs in one call.
    pub fn bootstrap_agent(&self, id: &str, config: &ResolvedAgentConfig) -> Result<()> {
        self.backend.bootstrap_agent(id, config)
    }

    /// Set the agent definition (name + prompt).
    pub fn set_agent(&self, id: &str, name: &str, prompt: &str) -> Result<()> {
        self.backend.set_agent(id, name, prompt)
    }

    /// Restart the agent process in the running sandbox.
    pub fn restart_agent(&self, id: &str, reason: &str) -> Result<serde_json::Value> {
        self.backend.restart_agent(id, reason)
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
