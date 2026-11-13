//! Event system for the TUI.

use std::time::Duration;

use ratatui::crossterm::event::{self, Event as CrosstermEvent};
use tokio::sync::mpsc;

use super::app::SandboxMetrics;
use super::host_capacity::HostCapacity;

/// Events that the TUI application can handle.
pub enum AppEvent {
    /// A terminal input event (key press, mouse, resize, etc.).
    Terminal(CrosstermEvent),
    /// Periodic tick for UI refresh.
    Tick,
    /// Sandbox creation started for a panel: index and status message.
    SandboxCreating {
        /// Stable panel id (never changes when other panels are removed).
        panel_id: u64,
        /// Human-readable status message.
        message: String,
    },
    /// Sandbox creation or startup failed.
    SandboxFailed {
        /// Stable panel id (never changes when other panels are removed).
        panel_id: u64,
        /// Error description.
        error: String,
    },
    /// SSH terminal connected and shell is active.
    SshConnected {
        /// Stable panel id (never changes when other panels are removed).
        panel_id: u64,
        /// Handle for sending keystrokes and resize events.
        handle: super::terminal::SshTerminalHandle,
    },
    /// Data received from SSH channel for a panel's terminal.
    TerminalData {
        /// Stable panel id (never changes when other panels are removed).
        panel_id: u64,
        /// Raw bytes from the SSH channel.
        data: Vec<u8>,
    },
    /// SSH connection failed or disconnected.
    SshDisconnected {
        /// Stable panel id (never changes when other panels are removed).
        panel_id: u64,
        /// Error description (None for clean disconnect).
        error: Option<String>,
    },
    /// A transient failure reading from the SSH console/exec channel (e.g. the
    /// event loop was briefly blocked). The sandbox may still be healthy, so
    /// the handler retries with backoff instead of treating this as a genuine
    /// disconnect.
    ConsoleReadError {
        /// Stable panel id (never changes when other panels are removed).
        panel_id: u64,
        /// Error description.
        error: String,
    },
    /// A supervised sandbox with this name is already running; attach via console.
    SupervisorReady {
        /// Stable panel id (never changes when other panels are removed).
        panel_id: u64,
        /// Supervisor sandbox name.
        name: String,
        /// Short identifier for display.
        short_id: String,
        /// Host-side project mount for gitsync / edit, if a project is set.
        project_mount: Option<sandbox::ProjectMount>,
        /// Interactive exec session: (host exec socket, agent program, args).
        /// `Some` means attach via the in-guest exec-agent PTY instead of the console.
        exec_pty: Option<(std::path::PathBuf, String, Vec<String>)>,
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
        /// Stable panel id (never changes when other panels are removed).
        panel_id: u64,
        /// Filename being uploaded.
        filename: String,
    },
    /// File upload to sandbox completed successfully.
    UploadComplete {
        /// Stable panel id (never changes when other panels are removed).
        panel_id: u64,
        /// Original filename.
        filename: String,
        /// Remote path inside the VM.
        remote_path: String,
        /// Bytes transferred.
        size: u64,
    },
    /// File upload to sandbox failed.
    UploadFailed {
        /// Stable panel id (never changes when other panels are removed).
        panel_id: u64,
        /// Error description.
        error: String,
    },
    /// Live resource samples for sandboxes, keyed by supervisor name.
    MetricsUpdated {
        /// (supervisor name, sample) pairs.
        samples: Vec<(String, SandboxMetrics)>,
    },
    /// Host capacity detected off the event loop (spawns `df`/`sysctl`).
    HostCapacityUpdated {
        /// Detected host resources.
        capacity: HostCapacity,
    },
    /// Sidebar file lists refreshed off the event loop (git subprocesses).
    SidebarFilesUpdated {
        /// Modified files (`git status --porcelain`) for the focused panel.
        modified: Vec<String>,
        /// Committed files (`git diff <base>..HEAD`) for the focused panel.
        committed: Vec<String>,
        /// Refusal message when the clone's `.git` is not a real directory.
        refusal: Option<String>,
    },
    /// Commit auto-sync pass completed off the event loop.
    CommitSyncResult(super::app::CommitSyncResult),
}

/// Spawn a background task that reads terminal events and forwards them to the channel.
pub fn spawn_terminal_event_reader(tx: mpsc::UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        loop {
            let events = tokio::task::spawn_blocking(|| {
                let mut batch: Vec<CrosstermEvent> = Vec::new();

                let has_first = match event::read() {
                    Ok(evt) => {
                        batch.push(evt);
                        true
                    }
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
