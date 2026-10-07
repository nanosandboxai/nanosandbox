//! Agent configuration types
//!
//! These types were extracted from the runtime's config module and represent
//! agent-specific configuration that lives in the sandbox SDK layer.

pub mod file;
pub mod models;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

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

// ---------------------------------------------------------------------------
// Per-agent compute defaults
// ---------------------------------------------------------------------------

/// Default compute resources for a specific agent type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentComputeDefaults {
    /// Number of virtual CPUs.
    pub cpus: u32,
    /// Memory in megabytes.
    pub memory_mb: u32,
}

/// Top-level structure for the agent_defaults.yaml file.
///
/// Example `~/.nanosandbox/agent_defaults.yaml`:
/// ```yaml
/// claude:
///   cpus: 2
///   memory_mb: 4096
/// codex:
///   cpus: 2
///   memory_mb: 2048
/// goose:
///   cpus: 2
///   memory_mb: 2048
/// cursor:
///   cpus: 2
///   memory_mb: 2048
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentDefaultsFile(pub HashMap<String, AgentComputeDefaults>);

/// Built-in compute defaults for each agent type.
///
/// These are the fallback values when no user override file exists.
/// Claude gets the highest allocation because its Node.js working set is
/// the largest (~1.5-2 GB); other agents are comfortable at 2 GB.
fn builtin_agent_defaults() -> HashMap<AgentType, AgentComputeDefaults> {
    let mut m = HashMap::new();
    m.insert(AgentType::Claude, AgentComputeDefaults { cpus: 2, memory_mb: 4096 });
    m.insert(AgentType::Codex, AgentComputeDefaults { cpus: 2, memory_mb: 2048 });
    m.insert(AgentType::Goose, AgentComputeDefaults { cpus: 2, memory_mb: 2048 });
    m.insert(AgentType::Cursor, AgentComputeDefaults { cpus: 2, memory_mb: 2048 });
    m
}

/// Returns the path to the user override file: `~/.nanosandbox/agent_defaults.yaml`.
pub fn agent_defaults_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".nanosandbox")
        .join("agent_defaults.yaml")
}

/// Load agent compute defaults, merging built-in values with any user overrides.
///
/// Resolution order (last wins):
/// 1. Built-in defaults (hardcoded per agent type)
/// 2. `~/.nanosandbox/agent_defaults.yaml` (user overrides)
///
/// Returns a map from `AgentType` to `AgentComputeDefaults`.
pub fn load_agent_defaults() -> HashMap<AgentType, AgentComputeDefaults> {
    let mut defaults = builtin_agent_defaults();

    // Merge user overrides if the file exists.
    let path = agent_defaults_path();
    if let Ok(contents) = std::fs::read_to_string(&path) {
        if let Ok(user_file) = serde_yaml::from_str::<AgentDefaultsFile>(&contents) {
            for (key, compute) in user_file.0 {
                if let Ok(agent_type) = key.parse::<AgentType>() {
                    defaults.insert(agent_type, compute);
                }
            }
        }
    }

    defaults
}

/// Look up compute defaults for a specific agent type.
///
/// Returns `None` if the agent type is unknown or not configured.
pub fn agent_compute_for(agent_type: AgentType) -> AgentComputeDefaults {
    load_agent_defaults()
        .remove(&agent_type)
        .unwrap_or(AgentComputeDefaults { cpus: 2, memory_mb: 2048 })
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
    /// Claude auto-invoke hint: shown in the skill description sent to the model.
    #[serde(default)]
    pub when_to_use: String,
    /// Tools pre-approved for this skill (Claude-specific).
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Whether the skill appears in the /skills user menu (None = treat as true).
    #[serde(default)]
    pub user_invocable: Option<bool>,
    /// Glob patterns — skill auto-attaches when matching files are in context.
    #[serde(default)]
    pub paths: Vec<String>,
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

/// Claude-specific settings written to ~/.claude/settings.json at bootstrap.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClaudeSettings {
    /// UI theme: "dark", "light", "dark-ansi", "light-ansi",
    /// "dark-colorblind", or "light-colorblind".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
}

impl ClaudeSettings {
    pub fn is_empty(&self) -> bool {
        self.theme.is_none()
    }
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
    /// Run the agent interactively on a TTY console (default false = headless).
    #[serde(default)]
    pub interactive: bool,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<AgentType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_settings: Option<ClaudeSettings>,
}

/// Agent-enriched sandbox configuration.
///
/// Wraps the runtime's `SandboxConfig` with agent-specific fields.
/// The runtime config handles VM resources (cpus, memory, mounts, network),
/// while this struct adds agent orchestration fields (MCP, skills, permissions).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSandboxConfig {
    /// VM-level sandbox configuration.
    pub sandbox: runtime::SandboxConfig,
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
    /// Run the agent interactively on a TTY console (default false = headless).
    pub interactive: bool,
    /// Agent permission level.
    pub permissions: Permissions,
    /// Task prompt for headless mode.
    pub prompt: Option<String>,
    /// Agent type (determines CLI command and config format).
    pub agent_type: Option<AgentType>,
    /// Model identifier (e.g., "claude-sonnet-4-5-20250929").
    pub model: Option<String>,
    /// Claude-specific settings (theme, etc.) written to ~/.claude/settings.json.
    pub claude_settings: Option<ClaudeSettings>,
}

impl Default for AgentSandboxConfig {
    fn default() -> Self {
        Self {
            sandbox: runtime::SandboxConfig::default(),
            mcp_servers: HashMap::new(),
            agent: None,
            skills: Vec::new(),
            resolved_agent: None,
            auto_mode: false,
            interactive: false,
            permissions: Permissions::Default,
            prompt: None,
            agent_type: None,
            model: None,
            claude_settings: None,
        }
    }
}

impl AgentSandboxConfig {
    pub fn builder() -> AgentSandboxConfigBuilder {
        AgentSandboxConfigBuilder::default()
    }
}

/// Builder for `AgentSandboxConfig`.
#[derive(Default)]
pub struct AgentSandboxConfigBuilder {
    config: AgentSandboxConfig,
}

impl AgentSandboxConfigBuilder {
    pub fn image(mut self, image: impl Into<String>) -> Self {
        self.config.sandbox.image = image.into();
        self
    }
    pub fn cpus(mut self, cpus: u32) -> Self {
        self.config.sandbox.cpus = cpus;
        self
    }
    pub fn memory_mb(mut self, mb: u32) -> Self {
        self.config.sandbox.memory_mb = mb;
        self
    }
    pub fn timeout_secs(mut self, secs: u32) -> Self {
        self.config.sandbox.timeout_secs = secs;
        self
    }
    pub fn run_as_root(mut self, run_as_root: bool) -> Self {
        self.config.sandbox.run_as_root = run_as_root;
        self
    }
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.sandbox.env.insert(key.into(), value.into());
        self
    }
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.config.sandbox.name = name.into();
        self
    }
    pub fn project(mut self, path: impl Into<std::path::PathBuf>, branch: Option<&str>) -> Self {
        self.config.sandbox.project = Some(runtime::ProjectConfig {
            path: path.into(),
            branch: branch.map(String::from),
            mount_point: "/workspace".to_string(),
            auto_sync: false,
        });
        self
    }
    pub fn agent_type(mut self, at: AgentType) -> Self {
        self.config.agent_type = Some(at);
        self
    }
    pub fn interactive(mut self, interactive: bool) -> Self {
        self.config.interactive = interactive;
        self
    }
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.config.model = Some(model.into());
        self
    }
    pub fn build(self) -> AgentSandboxConfig {
        self.config
    }
}

/// Default OCI registry for agent images.
pub const DEFAULT_AGENTS_REGISTRY: &str = "ghcr.io/nanosandboxai/agents-registry";

/// Known code-agent image names hosted in the agents registry.
const AGENT_IMAGES: &[&str] = &["base", "claude", "codex", "cursor", "goose", "legacy"];

/// Normalize an image reference.
///
/// - Known agent names (claude, codex, cursor, ...) are prefixed with the
///   agents registry (`ghcr.io/nanosandboxai/agents-registry/`).
/// - Other bare names (alpine, ubuntu, ...) are prefixed with Docker Hub
///   (`docker.io/library/`), matching standard Docker behaviour.
/// - Fully qualified references are returned as-is.
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
        if AGENT_IMAGES.contains(&name) {
            format!("{}/{}:{}", DEFAULT_AGENTS_REGISTRY, name, tag)
        } else {
            format!("docker.io/library/{}:{}", name, tag)
        }
    }
}
