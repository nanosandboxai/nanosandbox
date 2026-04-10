//! AgentSandbox - Agent-aware wrapper around the runtime Sandbox
//!
//! Provides agent-specific operations (MCP, skills, messaging) on top
//! of the generic HTTP API exposed by the runtime's Sandbox.

use std::collections::HashMap;

use nanosandbox::Sandbox;
use tracing::{debug, info};

use crate::config::{McpServerConfig, ResolvedAgentConfig, SkillDef};
use crate::error::{Error, Result};

/// Agent-aware sandbox wrapper.
///
/// Wraps a `nanosandbox::Sandbox` and adds agent-specific operations
/// using the generic gateway HTTP API.
pub struct AgentSandbox {
    /// The underlying runtime sandbox
    pub sandbox: Sandbox,
}

impl std::ops::Deref for AgentSandbox {
    type Target = Sandbox;
    fn deref(&self) -> &Sandbox {
        &self.sandbox
    }
}

impl std::ops::DerefMut for AgentSandbox {
    fn deref_mut(&mut self) -> &mut Sandbox {
        &mut self.sandbox
    }
}

impl AgentSandbox {
    /// Wrap an existing sandbox with agent capabilities.
    pub fn new(sandbox: Sandbox) -> Self {
        Self { sandbox }
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
            .gateway_http_post_sse("/api/v1/message", &body.to_string(), on_output)
            .map_err(Error::Runtime)
    }

    /// Bootstrap the agent with a full configuration (prompt, skills, MCP servers).
    pub fn bootstrap_agent(&self, config: &ResolvedAgentConfig) -> Result<()> {
        let body = serde_json::to_string(config)
            .map_err(|e| Error::Config(format!("Failed to serialize agent config: {}", e)))?;
        let (status, resp) = self
            .sandbox
            .gateway_http_post("/api/v1/agent/bootstrap", &body)
            .map_err(Error::Runtime)?;
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
    pub fn set_agent(&self, name: &str, prompt: &str) -> Result<()> {
        let body = serde_json::json!({
            "name": name,
            "prompt": prompt,
        });
        let (status, resp) = self
            .sandbox
            .gateway_http_post("/api/v1/agent", &body.to_string())
            .map_err(Error::Runtime)?;
        if status >= 400 {
            return Err(Error::Config(format!(
                "Set agent failed ({}): {}",
                status, resp
            )));
        }
        debug!("Agent set: {}", name);
        Ok(())
    }

    /// Restart the agent process.
    pub fn restart_agent(&self, reason: &str) -> Result<serde_json::Value> {
        let body = serde_json::json!({ "reason": reason });
        let (status, resp) = self
            .sandbox
            .gateway_http_post("/api/v1/agent/restart", &body.to_string())
            .map_err(Error::Runtime)?;
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
    pub fn add_mcp_server(&self, name: &str, config: McpServerConfig) -> Result<()> {
        let body = serde_json::json!({
            "name": name,
            "config": config,
        });
        let (status, resp) = self
            .sandbox
            .gateway_http_post("/api/v1/mcp/servers", &body.to_string())
            .map_err(Error::Runtime)?;
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
    pub fn remove_mcp_server(&self, name: &str) -> Result<()> {
        let path = format!("/api/v1/mcp/servers/{}", name);
        let (status, resp) = self
            .sandbox
            .gateway_http_delete(&path)
            .map_err(Error::Runtime)?;
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
    pub fn list_mcp_servers(&self) -> Result<HashMap<String, McpServerConfig>> {
        let (status, resp) = self
            .sandbox
            .gateway_http_get("/api/v1/mcp/servers")
            .map_err(Error::Runtime)?;
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
    pub fn enable_mcp_server(&self, name: &str) -> Result<()> {
        let path = format!("/api/v1/mcp/servers/{}/enable", name);
        let (status, resp) = self
            .sandbox
            .gateway_http_post(&path, "{}")
            .map_err(Error::Runtime)?;
        if status >= 400 {
            return Err(Error::McpServerError(format!(
                "Enable MCP server failed ({}): {}",
                status, resp
            )));
        }
        Ok(())
    }

    /// Disable an MCP server.
    pub fn disable_mcp_server(&self, name: &str) -> Result<()> {
        let path = format!("/api/v1/mcp/servers/{}/disable", name);
        let (status, resp) = self
            .sandbox
            .gateway_http_post(&path, "{}")
            .map_err(Error::Runtime)?;
        if status >= 400 {
            return Err(Error::McpServerError(format!(
                "Disable MCP server failed ({}): {}",
                status, resp
            )));
        }
        Ok(())
    }

    /// Add a skill to the running sandbox.
    pub fn add_skill(&self, skill: &SkillDef) -> Result<()> {
        let body = serde_json::to_string(skill)
            .map_err(|e| Error::SkillsError(format!("Failed to serialize skill: {}", e)))?;
        let (status, resp) = self
            .sandbox
            .gateway_http_post("/api/v1/skills", &body)
            .map_err(Error::Runtime)?;
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
    pub fn remove_skill(&self, name: &str) -> Result<()> {
        let path = format!("/api/v1/skills/{}", name);
        let (status, resp) = self
            .sandbox
            .gateway_http_delete(&path)
            .map_err(Error::Runtime)?;
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
    pub fn list_skills(&self) -> Result<HashMap<String, SkillDef>> {
        let (status, resp) = self
            .sandbox
            .gateway_http_get("/api/v1/skills")
            .map_err(Error::Runtime)?;
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
