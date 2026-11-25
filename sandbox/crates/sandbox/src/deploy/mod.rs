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

mod agent_cmd;
mod config_gen;
mod mount_planner;
mod skills_gen;

pub use agent_cmd::{AgentCommand, AgentCommandBuilder};
pub use config_gen::{ConfigFile, ConfigGenerator, McpConfigFormat};
pub use mount_planner::{
    agent_state_prefixes, merged_config_path, resolve_agent_config_host_path,
    validate_agent_config, MountPlanner, PlannedFile, PlannedMount, GUEST_HOME,
};
pub use skills_gen::SkillsGenerator;

use crate::config::{AgentSandboxConfig, AgentType, ResolvedAgentConfig};
use std::collections::HashMap;

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
    /// Inline agent-config files (absolute host paths under the sandbox state
    /// dir). Written after `config_files` so user overrides win.
    pub agent_config_files: Vec<PlannedFile>,
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
    ) -> Result<DeployPlan, String> {
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
        let skill_configs = SkillsGenerator::generate_all(
            &resolved.skills,
            &agent_type,
            &resolved.agent_name,
            &resolved.prompt,
        );
        config_files.extend(skill_configs);

        // Inline agent-config files: validated, resolved to absolute host paths
        // under the sandbox state dir, and appended after the generated config
        // files so user overrides win at the same guest path.
        let mut agent_config_files = Vec::new();
        if !config.agent_config.is_empty() {
            validate_agent_config(&config.agent_config, agent_type)
                .map_err(|e| format!("Sandbox '{}': {}", name, e))?;
            let mut keys: Vec<&String> = config.agent_config.keys().collect();
            keys.sort();
            for key in keys {
                let host_path = resolve_agent_config_host_path(sandbox_dir, key, agent_type)
                    .map_err(|e| format!("Sandbox '{}': {}", name, e))?;
                agent_config_files.push(PlannedFile {
                    host_path,
                    content: config.agent_config[key].clone().into_bytes(),
                });
            }
        }

        // claude.theme → ~/.claude/settings.json. Skipped when `agent_config`
        // already provides `.claude/settings.json` (user wins).
        if agent_type == AgentType::Claude {
            if let Some(theme) = config
                .claude_settings
                .as_ref()
                .and_then(|c| c.theme.clone())
            {
                let key = ".claude/settings.json";
                if !config.agent_config.contains_key(key) {
                    let content = serde_json::json!({ "theme": theme });
                    let content =
                        serde_json::to_string_pretty(&content).unwrap_or_else(|_| "{}".to_string());
                    let host_path = resolve_agent_config_host_path(sandbox_dir, key, agent_type)
                        .map_err(|e| format!("Sandbox '{}': {}", name, e))?;
                    agent_config_files.push(PlannedFile {
                        host_path,
                        content: content.into_bytes(),
                    });
                }
            }
        }

        // Agent command
        let agent_cmd = AgentCommandBuilder::build_with_session(
            &agent_type,
            &resolved.prompt,
            config.auto_mode,
            config.interactive,
            config.permissions,
            config.model.as_deref(),
            config.resume_session_id.as_deref(),
        );

        // Environment: start with sandbox env, overlay secrets (never persisted)
        let env = config.sandbox.env.clone();
        // Secrets are resolved at boot time by the supervisor and passed via
        // krun_set_env — they are NOT included in the plan's env map to
        // prevent accidental disk writes. The supervisor merges them at boot.

        Ok(DeployPlan {
            name,
            mounts,
            config_files,
            agent_config_files,
            agent_command: agent_cmd,
            env,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{McpServerConfig, Permissions, SkillDef};
    use std::collections::HashMap;

    fn sample_config() -> (AgentSandboxConfig, ResolvedAgentConfig) {
        let mut mcp = HashMap::new();
        mcp.insert(
            "github".to_string(),
            McpServerConfig {
                command: "npx".to_string(),
                args: vec![
                    "-y".to_string(),
                    "@modelcontextprotocol/server-github".to_string(),
                ],
                env: [("GITHUB_TOKEN".to_string(), "${GITHUB_TOKEN}".to_string())].into(),
                enabled: true,
            },
        );

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
            interactive: false,
            permissions: Permissions::Default,
            prompt: None,
            agent_type: Some(AgentType::Claude),
            model: Some("claude-sonnet-4-5-20250929".to_string()),
            claude_settings: None,
            resume_session_id: None,
            agent_config: HashMap::new(),
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
            interactive: false,
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
        let plan = DeployPlanner::plan(&config, &resolved, sandbox_dir, None).unwrap();

        assert_eq!(plan.name, "test-sandbox");
        assert!(
            !plan.mounts.is_empty(),
            "should have at least workspace mount"
        );
        assert!(!plan.config_files.is_empty(), "should have config files");
        assert_eq!(plan.agent_command.binary, "claude");
    }

    #[test]
    fn test_deploy_plan_agent_config_files_land_in_state_dir() {
        let (mut config, resolved) = sample_config();
        config.agent_type = Some(AgentType::OhMyOpenAgent);
        config.agent_config.insert(
            ".omo/omo.jsonc".to_string(),
            "{\"categories\":{\"default\":{\"model\":\"x\"}}}".to_string(),
        );
        let sandbox_dir = std::path::Path::new("/tmp/.nanosandbox/sandboxes/test-sandbox");
        let plan = DeployPlanner::plan(&config, &resolved, sandbox_dir, None).unwrap();

        assert_eq!(plan.agent_config_files.len(), 1);
        let file = &plan.agent_config_files[0];
        assert_eq!(
            file.host_path,
            sandbox_dir.join("state").join(".omo").join("omo.jsonc")
        );
        assert_eq!(
            String::from_utf8(file.content.clone()).unwrap(),
            "{\"categories\":{\"default\":{\"model\":\"x\"}}}"
        );
    }

    #[test]
    fn test_deploy_plan_agent_config_invalid_key_rejected() {
        let (mut config, resolved) = sample_config();
        config
            .agent_config
            .insert("../escape".to_string(), "x".to_string());
        let sandbox_dir = std::path::Path::new("/tmp/.nanosandbox/sandboxes/test-sandbox");
        let err = DeployPlanner::plan(&config, &resolved, sandbox_dir, None).unwrap_err();
        assert!(err.contains(".."), "got: {err}");
    }

    #[test]
    fn test_deploy_plan_claude_theme_generates_settings_json() {
        let (mut config, resolved) = sample_config();
        config.claude_settings = Some(crate::config::ClaudeSettings {
            theme: Some("dark".to_string()),
        });
        let sandbox_dir = std::path::Path::new("/tmp/.nanosandbox/sandboxes/test-sandbox");
        let plan = DeployPlanner::plan(&config, &resolved, sandbox_dir, None).unwrap();

        let theme_file = plan
            .agent_config_files
            .iter()
            .find(|f| f.host_path.ends_with(".claude/settings.json"))
            .expect("claude.theme must generate .claude/settings.json");
        let content = String::from_utf8(theme_file.content.clone()).unwrap();
        assert!(content.contains("\"theme\": \"dark\""), "got: {content}");
    }

    #[test]
    fn test_deploy_plan_agent_config_overrides_claude_theme() {
        let (mut config, resolved) = sample_config();
        config.claude_settings = Some(crate::config::ClaudeSettings {
            theme: Some("dark".to_string()),
        });
        config.agent_config.insert(
            ".claude/settings.json".to_string(),
            "{\"theme\": \"light\", \"extra\": true}".to_string(),
        );
        let sandbox_dir = std::path::Path::new("/tmp/.nanosandbox/sandboxes/test-sandbox");
        let plan = DeployPlanner::plan(&config, &resolved, sandbox_dir, None).unwrap();

        let settings = plan
            .agent_config_files
            .iter()
            .filter(|f| f.host_path.ends_with(".claude/settings.json"))
            .collect::<Vec<_>>();
        assert_eq!(
            settings.len(),
            1,
            "user agent_config must win; no generated theme file"
        );
        let content = String::from_utf8(settings[0].content.clone()).unwrap();
        assert!(content.contains("\"light\""), "got: {content}");
    }

    #[test]
    fn test_deploy_plan_agent_config_adds_no_extra_mount() {
        let (mut config, resolved) = sample_config();
        config.agent_type = Some(AgentType::OhMyOpenAgent);
        config
            .agent_config
            .insert(".omo/omo.jsonc".to_string(), "{}".to_string());
        let sandbox_dir = std::path::Path::new("/tmp/.nanosandbox/sandboxes/test-sandbox");
        let plan = DeployPlanner::plan(&config, &resolved, sandbox_dir, None).unwrap();

        // The file lands in the existing RW state mount; no new mount is added.
        assert!(
            !plan
                .mounts
                .iter()
                .any(|m| m.guest_path.contains("omo.jsonc")),
            "agent_config must not add a mount"
        );
        assert!(
            plan.mounts
                .iter()
                .any(|m| m.guest_path == "/home/developer/.omo"),
            "the .omo state mount must already exist"
        );
    }
}
