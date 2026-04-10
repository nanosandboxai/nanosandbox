//! Sandbox SDK - Agent-aware sandbox management
//!
//! This crate wraps the `nanosandbox` runtime with agent-specific
//! functionality including MCP server management, skill injection,
//! agent lifecycle control, and session management.
//!
//! # Architecture
//!
//! - `nanosandbox` (runtime repo): Pure VM engine - microVM, OCI images, containerization
//! - `sandbox` (this crate): Agent SDK - sandbox.yml, agent config, MCP, skills, sessions

#![allow(missing_docs)]
#![warn(clippy::all)]

pub mod agent_sandbox;
pub mod agents_registry;
pub mod config;
pub mod error;
pub mod session;
pub mod settings;

// Re-exports from runtime
pub use nanosandbox::{
    self, ExecOptions, ExecResult, ImageManager, ImageRef, Mount, MountType, NetworkConfig,
    NetworkMode, NetworkScope, OciBundle, PortMapping, ProjectConfig, ProgressFn, PulledImage,
    RegistryConfig, Runtime, Sandbox, SandboxConfig, SandboxInfo, SandboxRegistry, SandboxStatus,
    Stream,
};

// Re-exports from this crate
pub use agent_sandbox::AgentSandbox;
pub use agents_registry::AgentsRegistryClient;
pub use config::{
    AgentDefinition, AgentMcpRef, AgentSandboxConfig, AgentType, McpServerConfig, Permissions,
    ResolvedAgentConfig, SkillDef,
};
pub use config::file::{
    apply_cli_overrides, find_sandbox_file, load_sandbox_file, load_sandbox_files,
    resolve_sandbox_configs, SandboxFile,
};
pub use error::{Error, Result};
pub use session::Session;
pub use settings::UserSettings;
