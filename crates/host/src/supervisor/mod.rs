//! Supervisor state + control-socket protocol types (no FFI).

pub mod client;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Base directory for sandbox state: ~/.nanosandbox/sandboxes/{name}/
pub const SANDBOXES_DIR: &str = ".nanosandbox/sandboxes";

/// The nanosandbox state root: `$NANOSB_HOME` when set, else `~/.nanosandbox`.
pub fn nanosb_home() -> PathBuf {
    if let Ok(dir) = std::env::var("NANOSB_HOME") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".nanosandbox")
}

/// Base directory for sandbox state: `<NANOSB_HOME>/sandboxes`.
pub fn sandboxes_root() -> PathBuf {
    nanosb_home().join("sandboxes")
}

/// Sandbox state as seen by the supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxState {
    Starting,
    Running,
    Stopped,
    Error,
}

impl SandboxState {
    /// Whether the sandbox is live (`Starting`/`Running`), i.e. not terminal.
    pub fn is_active(self) -> bool {
        matches!(self, SandboxState::Starting | SandboxState::Running)
    }
}

/// Supervisor state persisted to `state.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SupervisorState {
    pub sandbox_name: String,
    pub state: SandboxState,
    pub pid: Option<i32>,
    pub exit_code: Option<i32>,
    pub started_at: String,
    pub config_hash: String,
    /// Unix epoch milliseconds when the sandbox reached a terminal state.
    #[serde(default)]
    pub exited_at_ms: Option<u64>,
    /// The supervisor process that owns this state. `reconcile` uses it to
    /// avoid finalizing a sandbox whose owner is still shutting it down.
    #[serde(default)]
    pub supervisor_pid: Option<i32>,
}

/// Request from a client to the supervisor.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlRequest {
    #[serde(rename_all = "snake_case")]
    Status {},
    #[serde(rename_all = "snake_case")]
    Stop { force: Option<bool> },
    #[serde(rename_all = "snake_case")]
    Attach {},
    #[serde(rename_all = "snake_case")]
    LogsFollow { tail: Option<usize> },
    #[serde(rename_all = "snake_case")]
    Ping {},
}

/// Response from the supervisor to a client.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlResponse {
    #[serde(rename_all = "snake_case")]
    Status {
        state: SandboxState,
        pid: Option<i32>,
        exit_code: Option<i32>,
        started_at: String,
        config_hash: String,
    },
    #[serde(rename_all = "snake_case")]
    Ok { version: String },
    #[serde(rename_all = "snake_case")]
    Error { message: String },
}

/// Frame sent to an attach client (output from VM).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttachFrame {
    #[serde(rename_all = "snake_case")]
    Output { data: String },
    #[serde(rename_all = "snake_case")]
    Exit { code: i32 },
}

/// Frame received from an attach client (input to VM).
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttachInput {
    #[serde(rename_all = "snake_case")]
    Input { data: String },
    #[serde(rename_all = "snake_case")]
    Resize { cols: u16, rows: u16 },
}
