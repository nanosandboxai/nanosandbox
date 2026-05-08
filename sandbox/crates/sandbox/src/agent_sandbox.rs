//! AgentSandbox - Agent-aware wrapper around the runtime Sandbox
//!
//! Provides agent-specific operations (MCP, skills, messaging) on top
//! of the gateway client managed by the sandbox layer.

use std::collections::HashMap;
use std::sync::Arc;

use runtime::ImageManager;
use tracing::{debug, info};

use crate::config::{AgentSandboxConfig, McpServerConfig, ResolvedAgentConfig, SkillDef};
use crate::error::{Error, Result};
use crate::sandbox::Sandbox as SandboxInner;

/// Agent-aware sandbox wrapper.
///
/// Wraps `crate::sandbox::Sandbox` (which itself wraps `runtime::Sandbox` and
/// manages the project mount and gateway client) and adds agent-specific
/// operations via the gateway HTTP API.
pub struct AgentSandbox {
    /// The intermediate sandbox (project-mount aware).
    pub sandbox: SandboxInner,
    /// The agent-level config used to create this sandbox.
    agent_config: AgentSandboxConfig,
}

impl std::ops::Deref for AgentSandbox {
    type Target = SandboxInner;
    fn deref(&self) -> &SandboxInner {
        &self.sandbox
    }
}

impl std::ops::DerefMut for AgentSandbox {
    fn deref_mut(&mut self) -> &mut SandboxInner {
        &mut self.sandbox
    }
}

/// Map a gateway error into our crate-level Error type.
fn gw_err(e: gateway::Error) -> Error {
    Error::Runtime(runtime::Error::ExecFailed(e.to_string()))
}

impl AgentSandbox {
    /// Wrap an existing intermediate sandbox with agent capabilities.
    pub fn new(sandbox: SandboxInner) -> Self {
        Self { sandbox, agent_config: AgentSandboxConfig::default() }
    }

    /// Create a new sandbox from an `AgentSandboxConfig`.
    pub async fn create(config: AgentSandboxConfig) -> runtime::Result<Self> {
        let sandbox = SandboxInner::create(config.sandbox.clone()).await?;
        Ok(Self { sandbox, agent_config: config })
    }

    /// Create a new sandbox using an existing `ImageManager`.
    pub async fn create_with_manager(
        config: AgentSandboxConfig,
        im: Arc<ImageManager>,
    ) -> runtime::Result<Self> {
        let sandbox = SandboxInner::create_with_manager(config.sandbox.clone(), im).await?;
        Ok(Self { sandbox, agent_config: config })
    }

    /// Return the agent-level config for this sandbox.
    pub fn config(&self) -> &AgentSandboxConfig {
        &self.agent_config
    }

    /// Destroy the sandbox, consuming it.
    pub async fn destroy(self) -> runtime::Result<()> {
        self.sandbox.destroy().await
    }

    /// Send a message to the agent running inside the sandbox (SSE streaming).
    pub fn send_message<F>(
        &self,
        message: &str,
        agent: &str,
        model: &str,
        env: &HashMap<String, String>,
        on_output: F,
    ) -> Result<i32>
    where
        F: Fn(&str, bool) + Send + Sync,
    {
        let body = serde_json::json!({
            "message": message,
            "agent": agent,
            "model": model,
            "env": env,
        });
        self.sandbox
            .gateway()
            .map_err(gw_err)?
            .http_post_sse("/api/v1/message", &body.to_string(), on_output)
            .map_err(gw_err)
    }

    /// Bootstrap the agent with a full configuration (prompt, skills, MCP servers).
    pub async fn bootstrap_agent(&self, config: &ResolvedAgentConfig) -> Result<()> {
        let body = serde_json::to_string(config)
            .map_err(|e| Error::Config(format!("Failed to serialize agent config: {}", e)))?;
        let (status, resp) = tokio::task::block_in_place(|| {
            self.sandbox
                .gateway()
                .map_err(gw_err)?
                .http_post("/api/v1/agent/bootstrap", &body)
                .map_err(gw_err)
        })?;
        if status >= 400 {
            return Err(Error::Config(format!(
                "Bootstrap failed ({}): {}",
                status, resp
            )));
        }
        info!("Agent bootstrapped: {}", config.agent_name);
        Ok(())
    }

    /// Set the agent definition (name + prompt).
    pub async fn set_agent(&self, name: &str, prompt: &str) -> Result<()> {
        let body = serde_json::json!({
            "name": name,
            "prompt": prompt,
        });
        let (status, resp) = tokio::task::block_in_place(|| {
            self.sandbox
                .gateway()
                .map_err(gw_err)?
                .http_post("/api/v1/agent", &body.to_string())
                .map_err(gw_err)
        })?;
        if status >= 400 {
            return Err(Error::Config(format!(
                "Set agent failed ({}): {}",
                status, resp
            )));
        }
        debug!("Agent set: {}", name);
        Ok(())
    }

    /// Get the agent type name (e.g. "claude", "goose", "codex").
    pub fn agent_type_name(&self) -> &str {
        self.agent_config
            .agent_type
            .as_ref()
            .map(|t| t.as_str())
            .unwrap_or("claude")
    }

    /// Restart the agent process.
    pub async fn restart_agent(&self, reason: &str) -> Result<serde_json::Value> {
        let body = serde_json::json!({ "agent": self.agent_type_name(), "reason": reason });
        let (status, resp) = tokio::task::block_in_place(|| {
            self.sandbox
                .gateway()
                .map_err(gw_err)?
                .http_post("/api/v1/agent/restart", &body.to_string())
                .map_err(gw_err)
        })?;
        if status >= 400 {
            return Err(Error::AgentRestartError(format!(
                "Restart failed ({}): {}",
                status, resp
            )));
        }
        serde_json::from_str(&resp).map_err(|e| {
            Error::AgentRestartError(format!("Failed to parse restart response: {}", e))
        })
    }

    /// Add or update an MCP server in the running sandbox.
    pub async fn add_mcp_server(&self, name: &str, config: McpServerConfig) -> Result<()> {
        let body = serde_json::json!({
            "name": name,
            "command": config.command,
            "args": config.args,
            "env": config.env,
            "enabled": config.enabled,
        });
        let (status, resp) = tokio::task::block_in_place(|| {
            self.sandbox
                .gateway()
                .map_err(gw_err)?
                .http_post("/api/v1/mcp/servers", &body.to_string())
                .map_err(gw_err)
        })?;
        if status >= 400 {
            return Err(Error::McpServerError(format!(
                "Add MCP server failed ({}): {}",
                status, resp
            )));
        }
        debug!("MCP server added: {}", name);
        Ok(())
    }

    /// Remove an MCP server from the running sandbox.
    pub async fn remove_mcp_server(&self, name: &str) -> Result<()> {
        let path = format!("/api/v1/mcp/servers/{}", name);
        let (status, resp) = tokio::task::block_in_place(|| {
            self.sandbox
                .gateway()
                .map_err(gw_err)?
                .http_delete(&path)
                .map_err(gw_err)
        })?;
        if status >= 400 {
            return Err(Error::McpServerError(format!(
                "Remove MCP server failed ({}): {}",
                status, resp
            )));
        }
        debug!("MCP server removed: {}", name);
        Ok(())
    }

    /// List all MCP servers in the running sandbox.
    pub async fn list_mcp_servers(&self) -> Result<HashMap<String, McpServerConfig>> {
        let (status, resp) = tokio::task::block_in_place(|| {
            self.sandbox
                .gateway()
                .map_err(gw_err)?
                .http_get("/api/v1/mcp/servers")
                .map_err(gw_err)
        })?;
        if status >= 400 {
            return Err(Error::McpServerError(format!(
                "List MCP servers failed ({}): {}",
                status, resp
            )));
        }
        serde_json::from_str(&resp).map_err(|e| {
            Error::McpServerError(format!("Failed to parse MCP servers: {}", e))
        })
    }

    /// Enable an MCP server.
    pub async fn enable_mcp_server(&self, name: &str) -> Result<()> {
        let path = format!("/api/v1/mcp/servers/{}/enable", name);
        let (status, resp) = tokio::task::block_in_place(|| {
            self.sandbox
                .gateway()
                .map_err(gw_err)?
                .http_post(&path, "{}")
                .map_err(gw_err)
        })?;
        if status >= 400 {
            return Err(Error::McpServerError(format!(
                "Enable MCP server failed ({}): {}",
                status, resp
            )));
        }
        Ok(())
    }

    /// Disable an MCP server.
    pub async fn disable_mcp_server(&self, name: &str) -> Result<()> {
        let path = format!("/api/v1/mcp/servers/{}/disable", name);
        let (status, resp) = tokio::task::block_in_place(|| {
            self.sandbox
                .gateway()
                .map_err(gw_err)?
                .http_post(&path, "{}")
                .map_err(gw_err)
        })?;
        if status >= 400 {
            return Err(Error::McpServerError(format!(
                "Disable MCP server failed ({}): {}",
                status, resp
            )));
        }
        Ok(())
    }

    /// Add a skill to the running sandbox.
    pub async fn add_skill(&self, skill: &SkillDef) -> Result<()> {
        let body = serde_json::to_string(skill)
            .map_err(|e| Error::SkillsError(format!("Failed to serialize skill: {}", e)))?;
        let (status, resp) = tokio::task::block_in_place(|| {
            self.sandbox
                .gateway()
                .map_err(gw_err)?
                .http_post("/api/v1/skills", &body)
                .map_err(gw_err)
        })?;
        if status >= 400 {
            return Err(Error::SkillsError(format!(
                "Add skill failed ({}): {}",
                status, resp
            )));
        }
        debug!("Skill added: {}", skill.name);
        Ok(())
    }

    /// Remove a skill from the running sandbox.
    pub async fn remove_skill(&self, name: &str) -> Result<()> {
        let path = format!("/api/v1/skills/{}", name);
        let (status, resp) = tokio::task::block_in_place(|| {
            self.sandbox
                .gateway()
                .map_err(gw_err)?
                .http_delete(&path)
                .map_err(gw_err)
        })?;
        if status >= 400 {
            return Err(Error::SkillsError(format!(
                "Remove skill failed ({}): {}",
                status, resp
            )));
        }
        debug!("Skill removed: {}", name);
        Ok(())
    }

    /// List all skills in the running sandbox.
    pub async fn list_skills(&self) -> Result<HashMap<String, SkillDef>> {
        let (status, resp) = tokio::task::block_in_place(|| {
            self.sandbox
                .gateway()
                .map_err(gw_err)?
                .http_get("/api/v1/skills")
                .map_err(gw_err)
        })?;
        if status >= 400 {
            return Err(Error::SkillsError(format!(
                "List skills failed ({}): {}",
                status, resp
            )));
        }
        serde_json::from_str(&resp)
            .map_err(|e| Error::SkillsError(format!("Failed to parse skills: {}", e)))
    }
}
