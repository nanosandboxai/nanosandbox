//! Sandbox SDK - Agent-aware sandbox management
//!
//! This crate wraps the `runtime` runtime with agent-specific
//! functionality including MCP server management, skill injection,
//! agent lifecycle control, and session management.
//!
//! # Architecture
//!
//! - `runtime` (runtime repo): Pure VM engine - microVM, OCI images, containerization
//! - `sandbox` (this crate): Agent SDK - sandbox.yml, agent config, MCP, skills, sessions

#![allow(missing_docs)]
#![warn(clippy::all)]

pub mod agent_sandbox;
pub mod agents_registry;
pub mod config;
pub mod error;
#[cfg(feature = "ffi")]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub mod ffi;
pub mod project;
pub mod sandbox;
pub mod session;
pub mod settings;
pub mod secrets;

// Re-exports from runtime (pure microVM types — no project/ssh/http/hvsocket here).
// Note: runtime::SandboxConfig is intentionally NOT re-exported at the top level;
// nanosandbox::SandboxConfig is the agent-layer AgentSandboxConfig (see below).
pub use runtime::{
    ImageManager, ImageRef, Mount, MountType, NetworkConfig,
    NetworkMode, NetworkScope, OciBundle, PortMapping, ProjectConfig, ProgressFn, PulledImage,
    RegistryConfig, Runtime, SandboxInfo, SandboxRegistry, SandboxStatus,
};

// Re-exports from gateway (exec types, secrets, client).
pub use gateway::{ExecOptions, ExecResult, GatewayClient, OutputChunk, Stream};
pub use gateway::{encrypt_payload, EncryptedPayload, SecretManifest};

// VM boot subprocess entry point (macOS HVF workaround — re-invokes nanosb as a
// clean single-threaded process before Tokio starts, so hv_vm_create() succeeds).
pub use runtime::runtime::handle_boot_vm_subprocess;

// System prerequisite validation (KVM/HVF availability checks).
pub mod validation {
    pub use runtime::runtime::validation::{validate_runtime_prerequisites_detailed, ValidationResult};
}

// The public Sandbox type is AgentSandbox — has MCP/skills/bootstrap methods.
// The inner vm-level sandbox is accessible via AgentSandbox::inner().
pub use crate::agent_sandbox::AgentSandbox as Sandbox;
pub use crate::project::{BranchStrategy, GitRepo, ProjectLayout, ProjectMount};

// Re-exports from this crate
pub use agents_registry::AgentsRegistryClient;
pub use config::{
    normalize_image, AgentDefinition, AgentMcpRef, AgentSandboxConfig, AgentSandboxConfigBuilder,
    AgentType, McpServerConfig, Permissions, ResolvedAgentConfig, SkillDef,
};
// SandboxConfig at the nanosandbox level = the agent-aware config (includes agent, skills, mcp etc.)
pub use config::AgentSandboxConfig as SandboxConfig;
pub use config::file::{
    apply_cli_overrides, find_sandbox_file, load_sandbox_file, load_sandbox_files,
    resolve_sandbox_configs, SandboxFile,
};
pub use error::{Error, Result};
pub use session::Session;
pub use settings::UserSettings;
