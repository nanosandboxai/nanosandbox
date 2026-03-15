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

pub use validation::{validate_runtime_prerequisites, validate_runtime_prerequisites_detailed, ValidationResult};

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use self::libkrun::LibkrunRuntime;

#[cfg(any(target_os = "macos", target_os = "linux"))]
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

#[cfg(target_os = "windows")]
pub use containerd_windows::{ContainerdWindowsRuntime, WindowsContainerdIsolation};

// Legacy exports (deprecated, kept for backward compatibility)
#[cfg(target_os = "windows")]
#[allow(deprecated)]
pub use windows::{WindowsContainerRuntime, WindowsIsolation};

use crate::config::{McpServerConfig, ResolvedAgentConfig, SkillDef};
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

    /// Get the SSH host port for a sandbox (if available).
    pub fn ssh_port(&self, id: &str) -> Option<u16> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.ssh_port(id),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = id;
            None
        }
    }

    /// Dynamically forward a guest port to the same host port via gvproxy.
    pub fn expose_port(&self, id: &str, port: u16) -> std::result::Result<(), String> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.expose_port(id, port),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = (id, port);
            Err("expose_port not supported on Windows".into())
        }
    }

    /// Get the SSH private key path for a sandbox (if available).
    pub fn ssh_key_path(&self, id: &str) -> Option<std::path::PathBuf> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.ssh_key_path(id),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = id;
            None
        }
    }

    /// Build a ready-to-use SSH command string for connecting to a sandbox.
    pub fn ssh_command(&self, id: &str) -> Option<String> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.ssh_command(id),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = id;
            None
        }
    }

    /// Push MCP server configurations to the gateway (bulk, on start).
    pub fn push_mcp_config(
        &self,
        id: &str,
        servers: &HashMap<String, McpServerConfig>,
    ) -> Result<()> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.push_mcp_config(id, servers),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = (id, servers);
            Err(crate::error::Error::McpNotSupported(
                "MCP not supported on Windows runtime yet".to_string(),
            ))
        }
    }

    /// Add or update an MCP server.
    pub fn add_mcp_server(
        &self,
        id: &str,
        name: &str,
        config: &McpServerConfig,
    ) -> Result<()> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.add_mcp_server(id, name, config),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = (id, name, config);
            Err(crate::error::Error::McpNotSupported(
                "MCP not supported on Windows runtime yet".to_string(),
            ))
        }
    }

    /// Remove an MCP server.
    pub fn remove_mcp_server(&self, id: &str, name: &str) -> Result<()> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.remove_mcp_server(id, name),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = (id, name);
            Err(crate::error::Error::McpNotSupported(
                "MCP not supported on Windows runtime yet".to_string(),
            ))
        }
    }

    /// List all MCP servers.
    pub fn list_mcp_servers(
        &self,
        id: &str,
    ) -> Result<HashMap<String, McpServerConfig>> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.list_mcp_servers(id),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = id;
            Err(crate::error::Error::McpNotSupported(
                "MCP not supported on Windows runtime yet".to_string(),
            ))
        }
    }

    /// Enable an MCP server.
    pub fn enable_mcp_server(&self, id: &str, name: &str) -> Result<()> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.enable_mcp_server(id, name),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = (id, name);
            Err(crate::error::Error::McpNotSupported(
                "MCP not supported on Windows runtime yet".to_string(),
            ))
        }
    }

    /// Disable an MCP server.
    pub fn disable_mcp_server(&self, id: &str, name: &str) -> Result<()> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.disable_mcp_server(id, name),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = (id, name);
            Err(crate::error::Error::McpNotSupported(
                "MCP not supported on Windows runtime yet".to_string(),
            ))
        }
    }

    /// Add a skill to the running sandbox.
    pub fn add_skill(&self, id: &str, skill: &SkillDef) -> Result<()> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.add_skill(id, skill),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = (id, skill);
            Err(crate::error::Error::SkillsError(
                "Skills not supported on Windows runtime yet".to_string(),
            ))
        }
    }

    /// Remove a skill from the running sandbox.
    pub fn remove_skill(&self, id: &str, name: &str) -> Result<()> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.remove_skill(id, name),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = (id, name);
            Err(crate::error::Error::SkillsError(
                "Skills not supported on Windows runtime yet".to_string(),
            ))
        }
    }

    /// List all skills in the running sandbox.
    pub fn list_skills(&self, id: &str) -> Result<HashMap<String, SkillDef>> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.list_skills(id),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = id;
            Err(crate::error::Error::SkillsError(
                "Skills not supported on Windows runtime yet".to_string(),
            ))
        }
    }

    /// Bootstrap agent definition + skills + MCPs in one call.
    pub fn bootstrap_agent(&self, id: &str, config: &ResolvedAgentConfig) -> Result<()> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.bootstrap_agent(id, config),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = (id, config);
            Err(crate::error::Error::SkillsError(
                "Agent bootstrap not supported on Windows runtime yet".to_string(),
            ))
        }
    }

    /// Set the agent definition (name + prompt).
    pub fn set_agent(&self, id: &str, name: &str, prompt: &str) -> Result<()> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.set_agent(id, name, prompt),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = (id, name, prompt);
            Err(crate::error::Error::SkillsError(
                "Agent definition not supported on Windows runtime yet".to_string(),
            ))
        }
    }

    /// Restart the agent process in the running sandbox.
    pub fn restart_agent(&self, id: &str, reason: &str) -> Result<serde_json::Value> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self {
                RuntimeBackend::Libkrun(r) => r.restart_agent(id, reason),
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = (id, reason);
            Err(crate::error::Error::AgentRestartError(
                "Agent restart not supported on Windows runtime yet".to_string(),
            ))
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
