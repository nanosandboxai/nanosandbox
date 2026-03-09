//! Event system for the TUI.

use std::sync::Arc;

use ratatui::crossterm::event::{self, Event as CrosstermEvent};
use tokio::sync::{mpsc, Mutex};

use crate::Sandbox;

/// Events that the TUI application can handle.
pub enum AppEvent {
    /// A terminal input event (key press, mouse, resize, etc.).
    Terminal(CrosstermEvent),
    /// Output from an agent: panel index, text content, and whether streaming is complete.
    AgentOutput(usize, String, bool),
    /// An agent process finished: panel index and exit code.
    AgentDone(usize, i32),
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
}

/// Spawn a background task that reads terminal events and forwards them to the channel.
pub fn spawn_terminal_event_reader(tx: mpsc::UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        loop {
            // Blocking read wrapped in spawn_blocking to avoid blocking the async runtime.
            let evt = tokio::task::spawn_blocking(event::read).await;

            match evt {
                Ok(Ok(crossterm_event)) => {
                    if tx.send(AppEvent::Terminal(crossterm_event)).is_err() {
                        // Receiver dropped; stop reading events.
                        break;
                    }
                }
                _ => {
                    // Error reading event or task panicked; stop.
                    break;
                }
            }
        }
    });
}
