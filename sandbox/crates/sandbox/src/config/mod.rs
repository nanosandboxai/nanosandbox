//! Agent configuration types
//!
//! These types were extracted from the runtime's config module and represent
//! agent-specific configuration that lives in the sandbox SDK layer.

pub mod file;
pub mod models;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Agent permission level controlling how much autonomy the agent has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Permissions {
    #[default]
    Default,
    AcceptEdits,
    AllowAll,
}

impl Permissions {
    pub fn effective(self, auto_mode: bool) -> Self {
        if auto_mode {
            Permissions::AllowAll
        } else {
            self
        }
    }
}

impl std::fmt::Display for Permissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Permissions::Default => write!(f, "default"),
            Permissions::AcceptEdits => write!(f, "accept_edits"),
            Permissions::AllowAll => write!(f, "allow_all"),
        }
    }
}

impl std::str::FromStr for Permissions {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "default" => Ok(Permissions::Default),
            "accept-edits" | "accept_edits" | "acceptEdits" => Ok(Permissions::AcceptEdits),
            "allow-all" | "allow_all" | "allowAll" => Ok(Permissions::AllowAll),
            _ => Err(format!(
                "unknown permission level '{}': expected default, accept-edits, or allow-all",
                s
            )),
        }
    }
}

/// The type of coding agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "lowercase")]
pub enum AgentType {
    Claude,
    Codex,
    Goose,
    Cursor,
}

impl AgentType {
    pub const ALL: &'static [AgentType] = &[
        AgentType::Claude,
        AgentType::Codex,
        AgentType::Goose,
        AgentType::Cursor,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            AgentType::Claude => "claude",
            AgentType::Codex => "codex",
            AgentType::Goose => "goose",
            AgentType::Cursor => "cursor",
        }
    }
}

impl std::fmt::Display for AgentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AgentType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude" | "claude-code" => Ok(AgentType::Claude),
            "codex" => Ok(AgentType::Codex),
            "goose" => Ok(AgentType::Goose),
            "cursor" | "cursor-agent" => Ok(AgentType::Cursor),
            _ => Err(format!(
                "unknown agent type '{}': expected claude, codex, goose, or cursor",
                s
            )),
        }
    }
}

fn default_enabled() -> bool {
    true
}

/// MCP server definition for agent tooling inside the sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

/// Skill definition resolved from registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub content: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// MCP reference within an agent definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMcpRef {
    pub name: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub package: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Agent definition resolved from registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub prompt: String,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub mcps: Vec<AgentMcpRef>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Fully resolved config bundle sent to gateway at boot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedAgentConfig {
    pub agent_name: String,
    pub prompt: String,
    pub skills: Vec<SkillDef>,
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
    #[serde(default)]
    pub auto_mode: bool,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<AgentType>,
}

/// Agent-enriched sandbox configuration.
///
/// Wraps the runtime's `SandboxConfig` with agent-specific fields.
/// The runtime config handles VM resources (cpus, memory, mounts, network),
/// while this struct adds agent orchestration fields (MCP, skills, permissions).
#[derive(Debug, Clone)]
pub struct AgentSandboxConfig {
    /// Runtime (VM) configuration.
    pub runtime: nanosandbox::SandboxConfig,
    /// MCP server definitions.
    pub mcp_servers: HashMap<String, McpServerConfig>,
    /// Agent definition name from registry.
    pub agent: Option<String>,
    /// Skill names from registry.
    pub skills: Vec<String>,
    /// Resolved agent config bundle (populated after registry resolution).
    pub resolved_agent: Option<ResolvedAgentConfig>,
    /// Enable auto/headless mode.
    pub auto_mode: bool,
    /// Agent permission level.
    pub permissions: Permissions,
    /// Task prompt for headless mode.
    pub prompt: Option<String>,
    /// Agent type (determines CLI command and config format).
    pub agent_type: Option<AgentType>,
    /// Model identifier (e.g., "claude-sonnet-4-5-20250929").
    pub model: Option<String>,
}

impl Default for AgentSandboxConfig {
    fn default() -> Self {
        Self {
            runtime: nanosandbox::SandboxConfig::default(),
            mcp_servers: HashMap::new(),
            agent: None,
            skills: Vec::new(),
            resolved_agent: None,
            auto_mode: false,
            permissions: Permissions::Default,
            prompt: None,
            agent_type: None,
            model: None,
        }
    }
}

/// Default OCI registry for agent images.
pub const DEFAULT_AGENTS_REGISTRY: &str = "ghcr.io/nanosandboxai/agents-registry";

/// Normalize an image reference: bare names are treated as agent names
/// and prefixed with the agents registry.
pub fn normalize_image(image: &str) -> String {
    if image.is_empty() {
        return image.to_string();
    }
    let name_part = image.split(':').next().unwrap_or(image);
    let has_registry =
        name_part.contains('/') || name_part.contains('.') || image.starts_with("localhost");
    if has_registry {
        image.to_string()
    } else {
        let (name, tag) = if let Some((n, t)) = image.split_once(':') {
            (n, t)
        } else {
            (image, "latest")
        };
        format!("{}/{}:{}", DEFAULT_AGENTS_REGISTRY, name, tag)
    }
}
