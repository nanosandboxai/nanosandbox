//! Sandbox configuration

pub mod file;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Configuration for creating a sandbox
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Unique name for the sandbox
    pub name: String,

    /// OCI image reference (e.g., "ghcr.io/devdone-labs/agents-registry/claude:latest")
    pub image: String,

    /// CPU cores to allocate
    #[serde(default = "default_cpus")]
    pub cpus: u32,

    /// Memory in MB
    #[serde(default = "default_memory")]
    pub memory_mb: u32,

    /// Mount points
    #[serde(default)]
    pub mounts: Vec<Mount>,

    /// Environment variables
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Network configuration
    #[serde(default)]
    pub network: NetworkConfig,

    /// Working directory inside sandbox
    #[serde(default = "default_workdir")]
    pub workdir: String,

    /// Timeout in seconds
    #[serde(default = "default_timeout")]
    pub timeout_secs: u32,

    /// MCP server configurations to push to the agent-gateway on start
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,

    /// Optional project mount configuration.
    #[serde(default)]
    pub project: Option<ProjectConfig>,

    /// Agent definition name from registry (e.g., "python-developer").
    #[serde(default)]
    pub agent: Option<String>,

    /// Additional skill names from registry.
    #[serde(default)]
    pub skills: Vec<String>,

    /// Fully resolved agent config (populated after registry resolution).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_agent: Option<ResolvedAgentConfig>,

    /// Enable auto mode (fully autonomous, no confirmation prompts).
    #[serde(default)]
    pub auto_mode: bool,
}

fn default_cpus() -> u32 {
    1
}

fn default_memory() -> u32 {
    512
}

fn default_workdir() -> String {
    "/workspace".to_string()
}

fn default_timeout() -> u32 {
    300
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            image: String::new(),
            cpus: default_cpus(),
            memory_mb: default_memory(),
            mounts: Vec::new(),
            env: HashMap::new(),
            network: NetworkConfig::default(),
            workdir: default_workdir(),
            timeout_secs: default_timeout(),
            mcp_servers: HashMap::new(),
            project: None,
            agent: None,
            skills: Vec::new(),
            resolved_agent: None,
            auto_mode: false,
        }
    }
}

/// Default OCI registry for agent images.
pub const DEFAULT_AGENTS_REGISTRY: &str = "ghcr.io/devdone-labs/agents-registry";

/// Normalize an image reference: bare names (no `/` or `.`) are treated as
/// agent names and prefixed with the agents registry instead of Docker Hub.
///
/// Examples:
/// - `"claude"` → `"ghcr.io/devdone-labs/agents-registry/claude:latest"`
/// - `"codex:v2"` → `"ghcr.io/devdone-labs/agents-registry/codex:v2"`
/// - `"ghcr.io/foo/bar:1.0"` → unchanged
/// - `"alpine:3.19"` → unchanged (contains `.`)
/// - `"localhost:5050/img"` → unchanged
pub fn normalize_image(image: &str) -> String {
    if image.is_empty() {
        return image.to_string();
    }
    // Extract the part before any `:` tag to check for registry indicators.
    let name_part = image.split(':').next().unwrap_or(image);
    let has_registry = name_part.contains('/') || name_part.contains('.') || image.starts_with("localhost");
    if has_registry {
        image.to_string()
    } else {
        // Bare name like "claude" or "codex:v2" — prefix with agents registry.
        let (name, tag) = if let Some((n, t)) = image.split_once(':') {
            (n, t)
        } else {
            (image, "latest")
        };
        format!("{}/{}:{}", DEFAULT_AGENTS_REGISTRY, name, tag)
    }
}

impl SandboxConfig {
    /// Create a new config builder
    pub fn builder() -> SandboxConfigBuilder {
        SandboxConfigBuilder::default()
    }
}

/// Builder for SandboxConfig
#[derive(Debug, Default)]
pub struct SandboxConfigBuilder {
    config: SandboxConfig,
}

impl SandboxConfigBuilder {
    /// Set the sandbox name
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.config.name = name.into();
        self
    }

    /// Set the OCI image
    pub fn image(mut self, image: impl Into<String>) -> Self {
        self.config.image = image.into();
        self
    }

    /// Set CPU cores
    pub fn cpus(mut self, cpus: u32) -> Self {
        self.config.cpus = cpus;
        self
    }

    /// Set memory in MB
    pub fn memory_mb(mut self, memory_mb: u32) -> Self {
        self.config.memory_mb = memory_mb;
        self
    }

    /// Add a bind mount point
    pub fn mount(mut self, host: impl Into<PathBuf>, container: impl Into<String>) -> Self {
        self.config.mounts.push(Mount::bind(host, container));
        self
    }

    /// Add a readonly bind mount point
    pub fn mount_readonly(
        mut self,
        host: impl Into<PathBuf>,
        container: impl Into<String>,
    ) -> Self {
        self.config
            .mounts
            .push(Mount::bind(host, container).readonly());
        self
    }

    /// Add a virtio-fs mount point (faster for VMs)
    pub fn mount_virtiofs(
        mut self,
        host: impl Into<PathBuf>,
        container: impl Into<String>,
    ) -> Self {
        self.config.mounts.push(Mount::virtiofs(host, container));
        self
    }

    /// Add a readonly virtio-fs mount point
    pub fn mount_virtiofs_readonly(
        mut self,
        host: impl Into<PathBuf>,
        container: impl Into<String>,
    ) -> Self {
        self.config
            .mounts
            .push(Mount::virtiofs(host, container).readonly());
        self
    }

    /// Set the network mode
    pub fn network_mode(mut self, mode: NetworkMode) -> Self {
        self.config.network.mode = mode;
        self
    }

    /// Add a port mapping
    pub fn port(mut self, host: u16, container: u16) -> Self {
        self.config
            .network
            .port_mappings
            .push(PortMapping::tcp(host, container));
        self
    }

    /// Add a DNS server
    pub fn dns(mut self, server: impl Into<String>) -> Self {
        self.config.network.dns.push(server.into());
        self
    }

    /// Add an environment variable
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.env.insert(key.into(), value.into());
        self
    }

    /// Set the working directory
    pub fn workdir(mut self, workdir: impl Into<String>) -> Self {
        self.config.workdir = workdir.into();
        self
    }

    /// Set the timeout
    pub fn timeout_secs(mut self, timeout: u32) -> Self {
        self.config.timeout_secs = timeout;
        self
    }

    /// Enable network access
    pub fn network_enabled(mut self, enabled: bool) -> Self {
        self.config.network.enabled = enabled;
        self
    }

    /// Set the TSI network scope (for libkrun direct FFI backend)
    pub fn network_scope(mut self, scope: NetworkScope) -> Self {
        self.config.network.scope = scope;
        self
    }

    /// Add an MCP server configuration
    pub fn mcp_server(mut self, name: impl Into<String>, config: McpServerConfig) -> Self {
        self.config.mcp_servers.insert(name.into(), config);
        self
    }

    /// Set a project directory to mount into the sandbox.
    pub fn project(mut self, path: impl Into<PathBuf>, branch: Option<&str>) -> Self {
        self.config.project = Some(ProjectConfig {
            path: path.into(),
            branch: branch.map(String::from),
            mount_point: "/workspace".to_string(),
            auto_sync: false,
        });
        self
    }

    /// Set a project directory with a specific branch name.
    pub fn project_with_branch(mut self, path: impl Into<PathBuf>, branch: &str) -> Self {
        self.config.project = Some(ProjectConfig {
            path: path.into(),
            branch: Some(branch.to_string()),
            mount_point: "/workspace".to_string(),
            auto_sync: false,
        });
        self
    }

    /// Set the agent definition name (from registry).
    pub fn agent(mut self, agent: impl Into<String>) -> Self {
        self.config.agent = Some(agent.into());
        self
    }

    /// Add a skill name (from registry).
    pub fn skill(mut self, name: impl Into<String>) -> Self {
        self.config.skills.push(name.into());
        self
    }

    /// Set the resolved agent config.
    pub fn resolved_agent(mut self, config: ResolvedAgentConfig) -> Self {
        self.config.resolved_agent = Some(config);
        self
    }

    /// Enable auto mode (fully autonomous, no confirmation prompts).
    pub fn auto_mode(mut self, enabled: bool) -> Self {
        self.config.auto_mode = enabled;
        self
    }

    /// Build the configuration
    pub fn build(self) -> SandboxConfig {
        self.config
    }
}

/// Mount type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MountType {
    /// Standard bind mount
    #[default]
    Bind,
    /// virtio-fs mount (VM-optimized, faster)
    VirtioFs,
}

/// Mount point configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mount {
    /// Path on the host
    pub host_path: PathBuf,

    /// Path inside the sandbox
    pub container_path: String,

    /// Whether the mount is read-only
    #[serde(default)]
    pub readonly: bool,

    /// Mount type (bind or virtio-fs)
    #[serde(default)]
    pub mount_type: MountType,
}

impl Mount {
    /// Create a new bind mount
    pub fn bind(host: impl Into<PathBuf>, container: impl Into<String>) -> Self {
        Self {
            host_path: host.into(),
            container_path: container.into(),
            readonly: false,
            mount_type: MountType::Bind,
        }
    }

    /// Create a new virtio-fs mount
    pub fn virtiofs(host: impl Into<PathBuf>, container: impl Into<String>) -> Self {
        Self {
            host_path: host.into(),
            container_path: container.into(),
            readonly: false,
            mount_type: MountType::VirtioFs,
        }
    }

    /// Set mount as read-only
    pub fn readonly(mut self) -> Self {
        self.readonly = true;
        self
    }
}

/// Port mapping for inbound connections
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortMapping {
    /// Port on the host
    pub host_port: u16,
    /// Port inside the container
    pub container_port: u16,
    /// Protocol (tcp/udp)
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

fn default_protocol() -> String {
    "tcp".to_string()
}

impl PortMapping {
    /// Create a TCP port mapping
    pub fn tcp(host: u16, container: u16) -> Self {
        Self {
            host_port: host,
            container_port: container,
            protocol: "tcp".to_string(),
        }
    }

    /// Create a UDP port mapping
    pub fn udp(host: u16, container: u16) -> Self {
        Self {
            host_port: host,
            container_port: container,
            protocol: "udp".to_string(),
        }
    }
}

/// Network configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Whether network access is enabled
    #[serde(default = "default_network_enabled")]
    pub enabled: bool,

    /// Network mode (tsi, bridge, none)
    #[serde(default = "default_network_mode")]
    pub mode: NetworkMode,

    /// TSI network scope (used by libkrun direct FFI backend)
    /// Controls what network destinations the VM can reach.
    #[serde(default = "default_network_scope")]
    pub scope: NetworkScope,

    /// Port mappings for inbound connections
    #[serde(default)]
    pub port_mappings: Vec<PortMapping>,

    /// DNS servers (uses host DNS if empty)
    #[serde(default)]
    pub dns: Vec<String>,
}

fn default_network_enabled() -> bool {
    true
}

fn default_network_mode() -> NetworkMode {
    NetworkMode::Tsi
}

fn default_network_scope() -> NetworkScope {
    NetworkScope::Any
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            enabled: default_network_enabled(),
            mode: default_network_mode(),
            scope: default_network_scope(),
            port_mappings: Vec::new(),
            dns: Vec::new(),
        }
    }
}

impl NetworkConfig {
    /// Create a config with no network access
    pub fn none() -> Self {
        Self {
            enabled: false,
            mode: NetworkMode::None,
            scope: NetworkScope::None,
            port_mappings: Vec::new(),
            dns: Vec::new(),
        }
    }

    /// Create a config with TSI networking (full internet access)
    pub fn tsi() -> Self {
        Self {
            enabled: true,
            mode: NetworkMode::Tsi,
            scope: NetworkScope::Any,
            port_mappings: Vec::new(),
            dns: Vec::new(),
        }
    }

    /// Add a port mapping
    pub fn with_port(mut self, host: u16, container: u16) -> Self {
        self.port_mappings.push(PortMapping::tcp(host, container));
        self
    }

    /// Add a DNS server
    pub fn with_dns(mut self, server: impl Into<String>) -> Self {
        self.dns.push(server.into());
        self
    }
}

/// Network mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    /// No network access
    None,

    /// Transparent Socket Impersonation (default)
    #[default]
    Tsi,

    /// Virtual bridge network
    Bridge,
}

/// TSI Network Scope for libkrun
///
/// Controls the level of network access the VM has when using
/// TSI (Transparent Socket Impersonation) via direct libkrun FFI.
///
/// libkrun enables TSI networking by default (when no explicit network
/// device is added). This enum allows configuring the desired scope
/// for future use with `krun_add_vsock` TSI feature flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NetworkScope {
    /// No outbound network access
    None,

    /// Local group only
    Group,

    /// Public internet access
    Public,

    /// Full access including localhost - recommended default
    #[default]
    Any,
}

/// Registry configuration for per-registry settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryConfig {
    /// Registry hostname (e.g., "ghcr.io")
    pub host: String,

    /// Allow insecure HTTP connections
    #[serde(default)]
    pub insecure: bool,

    /// Skip TLS certificate verification
    #[serde(default)]
    pub skip_tls_verify: bool,
}

impl RegistryConfig {
    /// Create a new registry config
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            insecure: false,
            skip_tls_verify: false,
        }
    }

    /// Allow insecure HTTP connections
    pub fn insecure(mut self) -> Self {
        self.insecure = true;
        self
    }

    /// Skip TLS verification
    pub fn skip_tls(mut self) -> Self {
        self.skip_tls_verify = true;
        self
    }
}

fn default_enabled() -> bool {
    true
}

/// MCP server definition for agent tooling inside the sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Command to run (e.g., "npx", "uvx")
    pub command: String,
    /// Arguments for the command
    pub args: Vec<String>,
    /// Environment variables for the server process
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Whether this server is enabled
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

/// Skill definition resolved from registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDef {
    /// Skill name (e.g., "tdd", "git-workflow")
    pub name: String,
    /// Human-readable description
    #[serde(default)]
    pub description: String,
    /// Full markdown content of the skill
    pub content: String,
    /// Version string (e.g., "1.0")
    #[serde(default)]
    pub version: String,
    /// Tags for categorization
    #[serde(default)]
    pub tags: Vec<String>,
}

/// MCP reference within an agent definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMcpRef {
    /// MCP server name
    pub name: String,
    /// Source registry (e.g., "smithery", "official")
    #[serde(default)]
    pub source: String,
    /// NPM/Python package name
    #[serde(default)]
    pub package: Option<String>,
    /// Environment variables for the MCP server
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Agent definition resolved from registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    /// Agent name (e.g., "python-developer")
    pub name: String,
    /// Human-readable description
    #[serde(default)]
    pub description: String,
    /// System prompt for the agent
    pub prompt: String,
    /// Skill names to resolve
    #[serde(default)]
    pub skills: Vec<String>,
    /// MCP references
    #[serde(default)]
    pub mcps: Vec<AgentMcpRef>,
    /// Tags for categorization
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Fully resolved config bundle sent to gateway at boot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedAgentConfig {
    /// Agent name
    pub agent_name: String,
    /// System prompt
    pub prompt: String,
    /// Resolved skill definitions
    pub skills: Vec<SkillDef>,
    /// MCP servers resolved from agent definition
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
    /// Enable auto mode (fully autonomous, no confirmation prompts).
    #[serde(default)]
    pub auto_mode: bool,
}

/// Configuration for mounting a project into the sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// Host path to the project directory.
    pub path: PathBuf,
    /// Optional branch name (auto-generated if None).
    pub branch: Option<String>,
    /// Mount point inside the VM (default: /workspace).
    #[serde(default = "default_project_mount_point")]
    pub mount_point: String,
    /// Whether auto-sync is enabled. When false, clones are created without source branches.
    #[serde(default)]
    pub auto_sync: bool,
}

fn default_project_mount_point() -> String {
    "/workspace".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_image_bare_agent_name() {
        assert_eq!(
            normalize_image("claude"),
            "ghcr.io/devdone-labs/agents-registry/claude:latest"
        );
        assert_eq!(
            normalize_image("codex"),
            "ghcr.io/devdone-labs/agents-registry/codex:latest"
        );
    }

    #[test]
    fn test_normalize_image_bare_name_with_tag() {
        assert_eq!(
            normalize_image("claude:v2"),
            "ghcr.io/devdone-labs/agents-registry/claude:v2"
        );
    }

    #[test]
    fn test_normalize_image_full_ref_unchanged() {
        assert_eq!(
            normalize_image("ghcr.io/devdone-labs/agents-registry/claude:latest"),
            "ghcr.io/devdone-labs/agents-registry/claude:latest"
        );
        assert_eq!(
            normalize_image("docker.io/library/alpine:3.19"),
            "docker.io/library/alpine:3.19"
        );
        assert_eq!(
            normalize_image("localhost:5050/agent-claude:latest"),
            "localhost:5050/agent-claude:latest"
        );
    }

    #[test]
    fn test_normalize_image_empty() {
        assert_eq!(normalize_image(""), "");
    }

    #[test]
    fn test_mcp_server_config_defaults() {
        let mcp = McpServerConfig {
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "@modelcontextprotocol/server-github".to_string()],
            env: HashMap::new(),
            enabled: true,
        };
        assert_eq!(mcp.command, "npx");
        assert!(mcp.enabled);
        assert!(mcp.env.is_empty());
    }

    #[test]
    fn test_mcp_server_config_serde_roundtrip() {
        let mcp = McpServerConfig {
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "@modelcontextprotocol/server-github".to_string()],
            env: HashMap::from([("GITHUB_TOKEN".to_string(), "abc123".to_string())]),
            enabled: true,
        };
        let json = serde_json::to_string(&mcp).unwrap();
        let parsed: McpServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.command, "npx");
        assert_eq!(parsed.args.len(), 2);
        assert_eq!(parsed.env.get("GITHUB_TOKEN").unwrap(), "abc123");
        assert!(parsed.enabled);
    }

    #[test]
    fn test_mcp_server_config_serde_defaults() {
        let json = r#"{"command":"npx","args":["-y","@upstash/context7-mcp"]}"#;
        let parsed: McpServerConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.enabled);
        assert!(parsed.env.is_empty());
    }

    #[test]
    fn test_sandbox_config_builder_with_mcp() {
        let config = SandboxConfig::builder()
            .name("test")
            .image("alpine:latest")
            .mcp_server("github", McpServerConfig {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "@modelcontextprotocol/server-github".to_string()],
                env: HashMap::new(),
                enabled: true,
            })
            .mcp_server("context7", McpServerConfig {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "@upstash/context7-mcp".to_string()],
                env: HashMap::new(),
                enabled: true,
            })
            .build();

        assert_eq!(config.mcp_servers.len(), 2);
        assert!(config.mcp_servers.contains_key("github"));
        assert!(config.mcp_servers.contains_key("context7"));
    }

    #[test]
    fn test_sandbox_config_default_no_mcp_servers() {
        let config = SandboxConfig::default();
        assert!(config.mcp_servers.is_empty());
    }

    #[test]
    fn test_mcp_config_json_matches_gateway_schema() {
        let config = McpServerConfig {
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "@modelcontextprotocol/server-github".to_string()],
            env: HashMap::from([
                ("GITHUB_TOKEN".to_string(), "test-token".to_string()),
            ]),
            enabled: true,
        };

        let json: serde_json::Value = serde_json::to_value(&config).unwrap();

        // Verify exact field names match gateway schema
        assert!(json.get("command").is_some(), "must have 'command' field");
        assert!(json.get("args").is_some(), "must have 'args' field");
        assert!(json.get("env").is_some(), "must have 'env' field");
        assert!(json.get("enabled").is_some(), "must have 'enabled' field");

        // Verify types
        assert!(json["command"].is_string());
        assert!(json["args"].is_array());
        assert!(json["env"].is_object());
        assert!(json["enabled"].is_boolean());

        // Verify values
        assert_eq!(json["command"], "npx");
        assert_eq!(json["args"][0], "-y");
        assert_eq!(json["env"]["GITHUB_TOKEN"], "test-token");
        assert_eq!(json["enabled"], true);
    }

    #[test]
    fn test_mcp_config_disabled_server() {
        let config = McpServerConfig {
            command: "uvx".to_string(),
            args: vec!["mcp-server-fetch".to_string()],
            env: HashMap::new(),
            enabled: false,
        };

        let json: serde_json::Value = serde_json::to_value(&config).unwrap();
        assert_eq!(json["enabled"], false);
        assert!(json["env"].as_object().unwrap().is_empty());
    }

    #[test]
    fn test_sandbox_config_with_project() {
        let config = SandboxConfig::builder()
            .name("test")
            .image("alpine")
            .project("/tmp/myproject", None)
            .build();

        assert!(config.project.is_some());
        let proj = config.project.unwrap();
        assert_eq!(proj.path, std::path::PathBuf::from("/tmp/myproject"));
        assert!(proj.branch.is_none());
        assert_eq!(proj.mount_point, "/workspace");
    }

    #[test]
    fn test_sandbox_config_with_project_and_branch() {
        let config = SandboxConfig::builder()
            .name("test")
            .image("alpine")
            .project_with_branch("/tmp/myproject", "feat/auth")
            .build();

        let proj = config.project.unwrap();
        assert_eq!(proj.branch, Some("feat/auth".to_string()));
    }

    #[test]
    fn test_sandbox_config_no_project() {
        let config = SandboxConfig::builder()
            .name("test")
            .image("alpine")
            .build();

        assert!(config.project.is_none());
    }

    #[test]
    fn test_sandbox_config_with_agent() {
        let config = SandboxConfig::builder()
            .name("test")
            .image("alpine")
            .agent("python-developer")
            .skill("tdd")
            .skill("git-workflow")
            .build();

        assert_eq!(config.agent, Some("python-developer".to_string()));
        assert_eq!(config.skills, vec!["tdd", "git-workflow"]);
        assert!(config.resolved_agent.is_none());
    }

    #[test]
    fn test_sandbox_config_with_resolved_agent() {
        let resolved = ResolvedAgentConfig {
            agent_name: "python-developer".to_string(),
            prompt: "You are a Python developer.".to_string(),
            skills: vec![SkillDef {
                name: "tdd".to_string(),
                description: "TDD".to_string(),
                content: "# TDD".to_string(),
                version: "1.0".to_string(),
                tags: vec![],
            }],
            mcp_servers: HashMap::new(),
            auto_mode: false,
        };

        let config = SandboxConfig::builder()
            .name("test")
            .image("alpine")
            .agent("python-developer")
            .resolved_agent(resolved)
            .build();

        assert!(config.resolved_agent.is_some());
        let r = config.resolved_agent.unwrap();
        assert_eq!(r.agent_name, "python-developer");
        assert_eq!(r.skills.len(), 1);
    }

    #[test]
    fn test_sandbox_config_default_no_agent() {
        let config = SandboxConfig::default();
        assert!(config.agent.is_none());
        assert!(config.skills.is_empty());
        assert!(config.resolved_agent.is_none());
    }
}
