//! Host-side config delivery for the zero-image-customization redesign.
//!
//! This module replaces the in-VM Go gateway's config generation with
//! host-side materialization. Given a resolved `AgentSandboxConfig` and
//! agent type, it produces:
//!
//! - A **mount plan** (workspace RW, agent state dirs RW, config/skill dirs RO)
//! - **Config file contents** (MCP server configs per agent format, skills files)
//! - **Agent command argv** (the CLI invocation to boot the agent)
//! - **Environment map** (resolved secrets + env vars, never written to disk)
//!
//! # Architecture
//!
//! ```text
//! AgentSandboxConfig + AgentType
//!         │
//!         ▼
//!   MountPlanner ──► Vec<PlannedMount>  (workspace RW, state RW, config RO)
//!         │
//!         ▼
//!   ConfigGenerator ──► Vec<ConfigFile>  (MCP JSON/YAML/TOML, skills files)
//!         │
//!         ▼
//!   AgentCommandBuilder ──► (argv, env)  (agent CLI invocation + env map)
//! ```
//!
//! # Conversion seam
//!
//! [`PlannedMount`] is a local type with a `readonly` field that `runtime::Mount`
//! does not expose as a separate concept (it has `readonly` on the struct).
//! Conversion to `runtime::Mount` is via `Into<runtime::Mount>`.

mod mount_planner;
mod config_gen;
mod skills_gen;
mod agent_cmd;

pub use mount_planner::{MountPlanner, PlannedMount};
pub use config_gen::{ConfigGenerator, ConfigFile, McpConfigFormat};
pub use skills_gen::SkillsGenerator;
pub use agent_cmd::{AgentCommandBuilder, AgentCommand};

use std::collections::HashMap;
use crate::config::{AgentSandboxConfig, AgentType, ResolvedAgentConfig};

/// Full deployment plan for a single sandbox.
///
/// Produced by [`DeployPlanner::plan`] and consumed by the supervisor
/// to set up virtiofs mounts, write config files, and boot the VM.
#[derive(Debug, Clone)]
pub struct DeployPlan {
    /// The sandbox name (from config).
    pub name: String,
    /// Mount plan: workspace RW, state RW, config RO.
    pub mounts: Vec<PlannedMount>,
    /// Config files to write to the host-side config directory.
    pub config_files: Vec<ConfigFile>,
    /// Agent command to boot inside the VM.
    pub agent_command: AgentCommand,
    /// Environment variables for the VM boot (includes resolved secrets).
    pub env: HashMap<String, String>,
}

/// Top-level planner that orchestrates mount planning, config generation,
/// and agent command building.
#[derive(Debug, Clone)]
pub struct DeployPlanner;

impl DeployPlanner {
    /// Produce a full [`DeployPlan`] from a resolved agent config.
    ///
    /// `sandbox_dir` is the host path `~/.nanosandbox/sandboxes/{name}/`.
    /// `workspace_host_path` is the host path for the workspace mount.
    pub fn plan(
        config: &AgentSandboxConfig,
        resolved: &ResolvedAgentConfig,
        sandbox_dir: &std::path::Path,
        workspace_host_path: Option<&std::path::Path>,
    ) -> DeployPlan {
        let agent_type = config.agent_type.unwrap_or(AgentType::Claude);
        let name = config.sandbox.name.clone();

        // Mount plan
        let mounts = MountPlanner::plan(config, &agent_type, sandbox_dir, workspace_host_path);

        // Config files (MCP + skills)
        let mut config_files = Vec::new();

        // MCP config files
        let mcp_configs = ConfigGenerator::generate_all(&resolved.mcp_servers, &agent_type);
        config_files.extend(mcp_configs);

        // Skills config files
        let skill_configs = SkillsGenerator::generate_all(&resolved.skills, &agent_type, &resolved.agent_name, &resolved.prompt);
        config_files.extend(skill_configs);

        // Agent command
        let agent_cmd = AgentCommandBuilder::build(
            &agent_type,
            &resolved.prompt,
            config.auto_mode,
            config.permissions,
            config.model.as_deref(),
        );

        // Environment: start with sandbox env, overlay secrets (never persisted)
        let env = config.sandbox.env.clone();
        // Secrets are resolved at boot time by the supervisor and passed via
        // krun_set_env — they are NOT included in the plan's env map to
        // prevent accidental disk writes. The supervisor merges them at boot.

        DeployPlan {
            name,
            mounts,
            config_files,
            agent_command: agent_cmd,
            env,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{McpServerConfig, Permissions, SkillDef};
    use std::collections::HashMap;

    fn sample_config() -> (AgentSandboxConfig, ResolvedAgentConfig) {
        let mut mcp = HashMap::new();
        mcp.insert("github".to_string(), McpServerConfig {
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "@modelcontextprotocol/server-github".to_string()],
            env: [("GITHUB_TOKEN".to_string(), "${GITHUB_TOKEN}".to_string())].into(),
            enabled: true,
        });

        let config = AgentSandboxConfig {
            sandbox: runtime::SandboxConfig {
                name: "test-sandbox".to_string(),
                image: "test:latest".to_string(),
                cpus: 2,
                memory_mb: 4096,
                ..runtime::SandboxConfig::default()
            },
            mcp_servers: mcp.clone(),
            agent: Some("python-developer".to_string()),
            skills: vec!["tdd".to_string()],
            resolved_agent: None,
            auto_mode: false,
            permissions: Permissions::Default,
            prompt: None,
            agent_type: Some(AgentType::Claude),
            model: Some("claude-sonnet-4-5-20250929".to_string()),
            claude_settings: None,
        };

        let resolved = ResolvedAgentConfig {
            agent_name: "python-developer".to_string(),
            prompt: "You are a Python developer.".to_string(),
            skills: vec![SkillDef {
                name: "tdd".to_string(),
                description: "Test-driven development".to_string(),
                content: "# TDD\n\nRed-green-refactor.".to_string(),
                version: "1.0".to_string(),
                tags: vec!["testing".to_string()],
                when_to_use: String::new(),
                allowed_tools: Vec::new(),
                user_invocable: None,
                paths: Vec::new(),
            }],
            mcp_servers: mcp,
            auto_mode: false,
            permissions: Permissions::Default,
            agent_type: Some(AgentType::Claude),
            claude_settings: None,
        };

        (config, resolved)
    }

    #[test]
    fn test_deploy_plan_basic() {
        let (config, resolved) = sample_config();
        let sandbox_dir = std::path::Path::new("/tmp/.nanosandbox/sandboxes/test-sandbox");
        let plan = DeployPlanner::plan(&config, &resolved, sandbox_dir, None);

        assert_eq!(plan.name, "test-sandbox");
        assert!(!plan.mounts.is_empty(), "should have at least workspace mount");
        assert!(!plan.config_files.is_empty(), "should have config files");
        assert_eq!(plan.agent_command.binary, "claude");
    }
}
