//! Wire protocol for the exec agent: length-prefixed JSON frames.
//!
//! Host -> guest frames are either an [`ExecRequest`] (start a command) or an
//! [`ExecControl`] (drive an already-running session: stdin, signal, resize).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Host -> guest: a command execution request.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExecRequest {
    /// Program to run (ignored when `shell` is true).
    pub command: String,
    /// Program arguments (ignored when `shell` is true).
    #[serde(default)]
    pub args: Vec<String>,
    /// Run `command` through `/bin/sh -c` instead of directly.
    #[serde(default)]
    pub shell: bool,
    /// Working directory.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Environment variables for the child process.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Kill the process after this many seconds (0 = no timeout).
    #[serde(default)]
    pub timeout_secs: u64,
    /// Allocate a PTY for the process (merges stdout/stderr, enables resize).
    #[serde(default)]
    pub tty: bool,
    /// Initial PTY size when `tty` is set.
    #[serde(default)]
    pub cols: u16,
    /// Initial PTY size when `tty` is set.
    #[serde(default)]
    pub rows: u16,
}

/// Host -> guest: control an in-flight session (sent as a subsequent frame).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecControl {
    /// Write bytes to the process stdin.
    Stdin { data: String },
    /// Close the process stdin (signal EOF to the child).
    StdinClose,
    /// Send a POSIX signal number to the process.
    Signal { signal: i32 },
    /// Resize the PTY.
    Resize { cols: u16, rows: u16 },
    /// Terminate the session (SIGKILL the process group).
    Kill,
}

/// Guest -> host: a streamed execution event.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecEvent {
    /// Process spawned; `pid` is the guest pid.
    Started { pid: u32 },
    /// A chunk of stdout.
    Stdout { data: String },
    /// A chunk of stderr.
    Stderr { data: String },
    /// Process exited with `code`.
    Exit { code: i32 },
    /// The agent could not run the request.
    Error { message: String },
}
