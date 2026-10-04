//! Event system for the TUI.

use std::sync::Arc;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event as CrosstermEvent};
use tokio::sync::{mpsc, Mutex};

use sandbox::Sandbox;

/// Events that the TUI application can handle.
pub enum AppEvent {
    /// A terminal input event (key press, mouse, resize, etc.).
    Terminal(CrosstermEvent),
    /// Periodic tick for UI refresh.
    Tick,
    /// Sandbox creation started for a panel: index and status message.
    SandboxCreating {
        /// Panel index.
        panel_idx: usize,
        /// Human-readable status message.
        message: String,
    },
    /// Sandbox successfully created and started.
    SandboxReady {
        /// Panel index.
        panel_idx: usize,
        /// Shared sandbox handle.
        sandbox: Arc<Mutex<Sandbox>>,
        /// Short sandbox identifier for display.
        short_id: String,
        /// Project mount transferred from the sandbox (if any).
        project_mount: Option<sandbox::ProjectMount>,
    },
    /// Sandbox creation or startup failed.
    SandboxFailed {
        /// Panel index.
        panel_idx: usize,
        /// Error description.
        error: String,
    },
    /// SSH terminal connected and shell is active.
    SshConnected {
        /// Panel index.
        panel_idx: usize,
        /// Handle for sending keystrokes and resize events.
        handle: super::terminal::SshTerminalHandle,
    },
    /// Data received from SSH channel for a panel's terminal.
    TerminalData {
        /// Panel index.
        panel_idx: usize,
        /// Raw bytes from the SSH channel.
        data: Vec<u8>,
    },
    /// SSH connection failed or disconnected.
    SshDisconnected {
        /// Panel index.
        panel_idx: usize,
        /// Error description (None for clean disconnect).
        error: Option<String>,
    },
    /// Open a TUI tool (suspend terminal, launch tool, resume on exit).
    OpenTuiTool {
        /// Binary name of the tool to launch.
        binary: String,
        /// Path to the clone directory to open.
        path: std::path::PathBuf,
    },
    /// File upload to sandbox started (for immediate feedback).
    UploadStarted {
        /// Panel index.
        panel_idx: usize,
        /// Filename being uploaded.
        filename: String,
    },
    /// File upload to sandbox completed successfully.
    UploadComplete {
        /// Panel index.
        panel_idx: usize,
        /// Original filename.
        filename: String,
        /// Remote path inside the VM.
        remote_path: String,
        /// Bytes transferred.
        size: u64,
    },
    /// File upload to sandbox failed.
    UploadFailed {
        /// Panel index.
        panel_idx: usize,
        /// Error description.
        error: String,
    },
}

/// Spawn a background task that reads terminal events and forwards them to the channel.
pub fn spawn_terminal_event_reader(tx: mpsc::UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        loop {
            let events = tokio::task::spawn_blocking(|| {
                let mut batch: Vec<CrosstermEvent> = Vec::new();

                let has_first = match event::read() {
                    Ok(evt) => { batch.push(evt); true }
                    Err(_) => false,
                };

                if has_first {
                    while let Ok(true) = event::poll(Duration::ZERO) {
                        match event::read() {
                            Ok(evt) => batch.push(evt),
                            Err(_) => break,
                        }
                    }
                }

                (has_first, batch)
            })
            .await;

            match events {
                Ok((true, batch)) => {
                    for evt in batch {
                        if tx.send(AppEvent::Terminal(evt)).is_err() {
                            return;
                        }
                    }
                }
                _ => {
                    break;
                }
            }
        }
    });
}

