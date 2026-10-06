//! AgentSandbox - Agent-aware wrapper around the runtime Sandbox
//!
//! Provides agent-specific operations on top of the sandbox layer.
//! Config delivery (MCP, skills, agent command) is handled by the
//! `deploy` module — this type is a thin wrapper for lifecycle.

use std::collections::HashMap;
use std::sync::Arc;

use runtime::ImageManager;
use crate::config::AgentSandboxConfig;
use crate::error::{Error, Result};
use crate::sandbox::Sandbox as SandboxInner;

/// Agent-aware sandbox wrapper.
///
/// Wraps `crate::sandbox::Sandbox` (which itself wraps `runtime::Sandbox` and
/// manages the project mount) and provides agent-specific operations.
///
/// Config delivery (MCP servers, skills, agent command) is performed by the
/// [`crate::deploy`] module at deploy time — no in-VM CRUD APIs remain.
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

    /// Return a host-side exec client for this sandbox (next mode).
    ///
    /// The sandbox must have been started with a vsock bridge
    /// (`SandboxConfig::vsock_bridge`); otherwise the returned client reports
    /// `NotAvailable` on use.
    pub fn exec_client(&self) -> runtime::exec::ExecClient {
        self.sandbox.exec_client()
    }

    /// Run a command in the sandbox, returning buffered output.
    pub fn exec(
        &self,
        command: &str,
        args: &[&str],
    ) -> Result<runtime::exec::ExecResult> {
        self.exec_client()
            .exec(command, args)
            .map_err(|e| Error::Runtime(runtime::Error::ExecFailed(e.to_string())))
    }

    /// Run a command in the sandbox with streaming output.
    pub fn exec_stream<F>(
        &self,
        command: &str,
        args: &[&str],
        options: runtime::exec::ExecOptions,
        on_chunk: F,
    ) -> Result<i32>
    where
        F: FnMut(runtime::exec::OutputChunk),
    {
        self.exec_client()
            .exec_stream(command, args, options, on_chunk)
            .map_err(|e| Error::Runtime(runtime::Error::ExecFailed(e.to_string())))
    }

    /// Run a shell command (`/bin/sh -c`) in the sandbox.
    pub fn shell(&self, script: &str) -> Result<runtime::exec::ExecResult> {
        self.exec_client()
            .exec_with(script, &[], runtime::exec::ExecOptions::new().shell(true))
            .map_err(|e| Error::Runtime(runtime::Error::ExecFailed(e.to_string())))
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
            .map_err(|e| Error::Runtime(runtime::Error::ExecFailed(e.to_string())))?
            .http_post_sse("/api/v1/message", &body.to_string(), on_output)
            .map_err(|e| Error::Runtime(runtime::Error::ExecFailed(e.to_string())))
    }

    /// Get the agent type name (e.g. "claude", "goose", "codex").
    pub fn agent_type_name(&self) -> &str {
        self.agent_config
            .agent_type
            .as_ref()
            .map(|t| t.as_str())
            .unwrap_or("claude")
    }
}
