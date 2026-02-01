//! Sandbox management

use crate::config::SandboxConfig;
use crate::error::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Sandbox lifecycle status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxStatus {
    /// Sandbox is being created
    #[default]
    Creating,
    /// Sandbox is running
    Running,
    /// Sandbox is stopped
    Stopped,
    /// Sandbox encountered an error
    Error,
}

/// Result of command execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    /// Exit code
    pub exit_code: i32,
    /// Standard output
    pub stdout: String,
    /// Standard error
    pub stderr: String,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
}

impl ExecResult {
    /// Check if the command succeeded
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Output stream type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// Standard output
    Stdout,
    /// Standard error
    Stderr,
}

/// Chunk of streaming output
#[derive(Debug, Clone)]
pub struct OutputChunk {
    /// The stream this chunk came from
    pub stream: Stream,
    /// The data
    pub data: String,
    /// Timestamp
    pub timestamp: DateTime<Utc>,
}

/// A managed sandbox instance
pub struct Sandbox {
    /// Unique identifier
    id: String,
    /// Configuration
    config: SandboxConfig,
    /// Current status
    status: SandboxStatus,
    /// Runtime container/VM ID
    runtime_id: Option<String>,
}

impl Sandbox {
    /// Create a new sandbox (does not start it)
    pub async fn create(config: SandboxConfig) -> Result<Self> {
        let id = uuid::Uuid::new_v4().to_string();

        // TODO: Pull image if needed
        // TODO: Create OCI bundle
        // TODO: Initialize crun/libkrun

        Ok(Self {
            id,
            config,
            status: SandboxStatus::Creating,
            runtime_id: None,
        })
    }

    /// Get the sandbox ID
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Get the sandbox configuration
    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }

    /// Get the current status
    pub fn status(&self) -> SandboxStatus {
        self.status
    }

    /// Start the sandbox
    pub async fn start(&mut self) -> Result<()> {
        // TODO: Start the VM via crun/libkrun
        self.status = SandboxStatus::Running;
        Ok(())
    }

    /// Execute a command in the sandbox
    pub async fn exec(&self, command: &str, args: &[&str]) -> Result<ExecResult> {
        let _start = std::time::Instant::now();

        // TODO: Execute via crun exec

        Ok(ExecResult {
            exit_code: 0,
            stdout: format!("Executed: {} {:?}", command, args),
            stderr: String::new(),
            duration_ms: 0,
        })
    }

    /// Execute a command with streaming output
    pub async fn exec_stream<F>(&self, _command: &str, mut _on_output: F) -> Result<i32>
    where
        F: FnMut(OutputChunk) + Send,
    {
        // TODO: Implement streaming exec
        Ok(0)
    }

    /// Stop the sandbox
    pub async fn stop(&mut self) -> Result<()> {
        // TODO: Stop the VM
        self.status = SandboxStatus::Stopped;
        Ok(())
    }

    /// Destroy the sandbox
    pub async fn destroy(mut self) -> Result<()> {
        self.stop().await?;
        // TODO: Remove OCI bundle and cleanup
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_sandbox_create() {
        let config = SandboxConfig::builder()
            .name("test")
            .image("alpine:latest")
            .build();

        let sandbox = Sandbox::create(config).await.unwrap();
        assert!(!sandbox.id().is_empty());
        assert_eq!(sandbox.status(), SandboxStatus::Creating);
    }
}
