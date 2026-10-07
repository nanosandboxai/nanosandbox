//! Error types for the Sandbox SDK

use thiserror::Error;

/// Sandbox SDK error type
#[derive(Debug, Error)]
pub enum Error {
    /// MCP not supported (sandbox not in persistent/gateway mode)
    #[error("MCP operations require a supervisor-managed sandbox")]
    McpNotSupported,

    /// MCP server operation failed
    #[error("MCP server error: {0}")]
    McpServerError(String),

    /// Skills operation failed
    #[error("Skills error: {0}")]
    SkillsError(String),

    /// Agent restart failed
    #[error("Agent restart failed: {0}")]
    AgentRestartError(String),

    /// Runtime error (from runtime)
    #[error("Runtime error: {0}")]
    Runtime(#[from] runtime::Error),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),
}

/// Result type alias
pub type Result<T> = std::result::Result<T, Error>;
