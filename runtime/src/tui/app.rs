//! Application state for the TUI.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::Sandbox;

use ratatui::layout::Rect;

use super::commands::{self, Command, ParseResult};
use super::terminal::{SshTerminal, SshTerminalHandle};
use super::text_input::TextInput;

/// State for an active mouse text selection within a panel.
#[derive(Debug, Clone)]
pub struct MouseSelection {
    /// Index of the panel where the selection started.
    pub panel_idx: usize,
    /// Starting position in terminal coordinates (row, col) relative to the
    /// panel's inner area (i.e., vt100 screen coordinates).
    pub start: (u16, u16),
    /// Current end position in terminal coordinates (row, col).
    pub end: (u16, u16),
    /// Whether the mouse button is still held (drag in progress).
    pub dragging: bool,
}

impl MouseSelection {
    /// Return (start, end) normalized so start comes before end in row-major order.
    pub fn normalized(&self) -> ((u16, u16), (u16, u16)) {
        if self.start.0 < self.end.0
            || (self.start.0 == self.end.0 && self.start.1 <= self.end.1)
        {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }
}

/// Operating mode for a panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelMode {
    /// Loading animation — sandbox is booting or SSH is connecting.
    Loading,
    /// Embedded SSH terminal — keystrokes forwarded to remote PTY.
    Terminal,
}

/// Where keyboard input is currently directed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFocus {
    /// Input goes to the global command bar.
    Global,
    /// Input goes to the focused panel's input bar.
    Panel,
}

/// Which file list tab is active in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarFilesTab {
    /// Uncommitted changes (git status --porcelain).
    Modified,
    /// Files changed by sandbox commits vs the branch starting point.
    Committed,
}

/// Role of a chat message sender.
#[derive(Debug, Clone, PartialEq)]
pub enum MessageRole {
    /// Message from the user.
    User,
    /// Message from an agent.
    Agent,
    /// System-generated message.
    System,
}

/// A single chat message in a panel.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// Who sent this message.
    pub role: MessageRole,
    /// The text content of the message.
    pub content: String,
}

/// Result of submitting input (pressing Enter).
#[derive(Debug)]
pub enum SubmitResult {
    /// Regular message to send to the focused agent
    Message(String),
    /// Parsed slash command
    Command(Command),
    /// Slash command parse error with help message
    CommandError(String),
    /// Empty input, do nothing
    Empty,
    /// No panel focused
    NoPanel,
}

/// State for a single agent panel.
pub struct AgentPanel {
    /// Agent type key (e.g. "claude", "codex") — used for CLI command resolution.
    pub agent_name: String,
    /// Display name shown in panel title. Falls back to agent_name if not set.
    pub display_name: Option<String>,
    /// The sandbox instance backing this agent, wrapped in Arc<Mutex<>> for
    /// shared access between the event loop and background streaming tasks.
    pub sandbox: Option<Arc<Mutex<Sandbox>>>,
    /// Chat history for this panel (kept for /copy, but not rendered in panels).
    pub chat_history: Vec<ChatMessage>,
    /// Current input buffer with cursor tracking and multiline support.
    pub input: TextInput,
    /// Short identifier for the sandbox.
    pub sandbox_id_short: String,
    /// Per-panel environment variables (e.g., API keys).
    /// Merged with host env when sending messages (panel env takes priority).
    pub env: HashMap<String, String>,
    /// Current operating mode of the panel.
    pub mode: PanelMode,
    /// Last rendered input width (set by renderer, used by key handler for cursor movement).
    pub last_input_width: u16,
    /// Embedded SSH terminal state (vt100 parser + screen buffer).
    pub terminal: Option<SshTerminal>,
    /// Handle for sending keystrokes and resize events to the SSH session.
    pub terminal_handle: Option<SshTerminalHandle>,
    /// Last rendered terminal area (cols, rows) for resize detection.
    pub last_terminal_size: (u16, u16),
    /// URL dedup keys already opened in the host browser.
    /// For auth URLs the key is host-only; for others it's host+path.
    pub opened_urls: HashSet<String>,
    /// Pending URLs waiting to be opened after a short debounce delay.
    /// Key: dedup key, Value: (full URL, timestamp of last growth).
    pub pending_urls: HashMap<String, (String, std::time::Instant)>,
    /// Active project mount for this panel's sandbox.
    pub project_mount: Option<crate::project::ProjectMount>,
    /// Last known HEAD SHA in the clone (for commit auto-sync detection).
    pub last_known_head: Option<String>,
    /// Initial HEAD SHA of the clone at creation (base for committed files diff).
    pub base_commit: Option<String>,
    /// Per-panel sync override. Takes priority over global settings.
    /// None = use global, Some(true) = force on, Some(false) = force off.
    pub sync_override: Option<bool>,
    /// SSH host port for port forwarding (stored from SandboxReady).
    pub ssh_host_port: Option<u16>,
    /// SSH private key path for port forwarding (stored from SandboxReady).
    pub ssh_key_path: Option<PathBuf>,
    /// Guest ports with active SSH local-port-forwards (`ssh -L`).
    pub forwarded_ports: HashSet<u16>,
    /// SSH port-forward child processes (killed on panel close).
    pub port_forward_children: Vec<std::process::Child>,
    /// Animation tick counter for the loading border sweep.
    pub loading_tick: u16,
    /// Error message shown below the logo when sandbox creation or SSH fails.
    pub loading_error: Option<String>,
    /// Whether a reconnect is in progress (suppresses auto-kill on disconnect).
    pub reconnecting: bool,
}

impl AgentPanel {
    /// Create a new agent panel with the given name.
    pub fn new(agent_name: &str) -> Self {
        Self {
            agent_name: agent_name.to_string(),
            display_name: None,
            sandbox: None,
            chat_history: Vec::new(),
            input: TextInput::new(),
            sandbox_id_short: String::new(),
            env: HashMap::new(),
            mode: PanelMode::Loading,
            last_input_width: 40,
            terminal: None,
            terminal_handle: None,
            last_terminal_size: (80, 24),
            opened_urls: HashSet::new(),
            pending_urls: HashMap::new(),
            project_mount: None,
            last_known_head: None,
            base_commit: None,
            sync_override: None,
            ssh_host_port: None,
            ssh_key_path: None,
            forwarded_ports: HashSet::new(),
            port_forward_children: Vec::new(),
            loading_tick: 0,
            loading_error: None,
            reconnecting: false,
        }
    }
}

/// Top-level application state.
pub struct App {
    /// The set of agent panels.
    pub panels: Vec<AgentPanel>,
    /// Index of the currently focused panel.
    pub focused_panel: usize,
    /// Whether the application should exit.
    pub should_quit: bool,
    /// Whether the MCP sidebar is visible.
    pub show_mcp_sidebar: bool,
    /// Whether the sandbox sidebar is visible.
    pub show_sandbox_sidebar: bool,
    /// Whether a panel is zoomed to full width.
    pub zoomed: bool,
    /// Whether the welcome screen is visible.
    pub show_welcome: bool,
    /// Global input buffer with cursor tracking and multiline support.
    pub global_input: TextInput,
    /// System messages displayed on the welcome screen (e.g. /help output).
    pub system_messages: Vec<ChatMessage>,
    /// Currently selected autocomplete index (None = no selection).
    pub autocomplete_index: Option<usize>,
    /// Whether input is directed to the global bar or the focused panel.
    pub input_focus: InputFocus,
    /// Last rendered global input width (set by renderer, used by key handler).
    pub last_global_input_width: u16,
    /// Project path to mount into sandboxes (if specified on launch).
    pub project_path: Option<std::path::PathBuf>,
    /// Scroll offset for the sandbox list section of the sidebar.
    pub sidebar_sandbox_scroll: usize,
    /// Scroll offset for the modified files section of the sidebar.
    pub sidebar_files_scroll: usize,
    /// Which sidebar section has focus (false = sandboxes, true = files).
    pub sidebar_files_focused: bool,
    /// Cached list of modified files for the focused panel's project mount.
    pub sidebar_modified_files: Vec<String>,
    /// Cached list of committed files for the focused panel's project mount.
    pub sidebar_committed_files: Vec<String>,
    /// Active tab in the files section of the sidebar.
    pub sidebar_files_tab: SidebarFilesTab,
    /// Scroll offset for the committed files section.
    pub sidebar_committed_scroll: usize,
    /// Tick counter for throttling sidebar refresh.
    pub sidebar_tick_counter: u8,
    /// Persistent user settings (loaded from ~/.nanosandbox/config.toml).
    pub settings: crate::settings::UserSettings,
    /// Temporary status message shown on the status bar (message, remaining ticks).
    pub status_message: Option<(String, u8)>,
    /// Active colour theme (resolved at startup, switchable via `/theme`).
    pub theme: &'static super::theme::Theme,
    /// Name of the active theme (for display and persistence).
    pub theme_name: super::theme::ThemeName,
    /// Active mouse text selection (at most one at a time across all panels).
    pub mouse_selection: Option<MouseSelection>,
    /// Cached panel inner areas from the last render, used to map mouse
    /// coordinates to panel-relative terminal positions.
    pub panel_areas: Vec<(usize, Rect)>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// Create a new application with default state.
    pub fn new() -> Self {
        let settings = crate::settings::UserSettings::load();
        let (theme, theme_name) = super::theme::Theme::resolve(&settings.ui.theme);
        Self {
            panels: Vec::new(),
            focused_panel: 0,
            should_quit: false,
            show_mcp_sidebar: false,
            show_sandbox_sidebar: false,
            zoomed: false,
            show_welcome: true,
            global_input: TextInput::new(),
            system_messages: Vec::new(),
            autocomplete_index: None,
            input_focus: InputFocus::Global,
            last_global_input_width: 40,
            project_path: None,
            sidebar_sandbox_scroll: 0,
            sidebar_files_scroll: 0,
            sidebar_files_focused: false,
            sidebar_modified_files: Vec::new(),
            sidebar_committed_files: Vec::new(),
            sidebar_files_tab: SidebarFilesTab::Modified,
            sidebar_committed_scroll: 0,
            sidebar_tick_counter: 0,
            settings,
            status_message: None,
            theme,
            theme_name,
            mouse_selection: None,
            panel_areas: Vec::new(),
        }
    }

    /// Set a temporary status message that appears on the status bar for ~3 seconds.
    pub fn set_status_message(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), 12)); // 12 ticks × 250ms = 3s
    }

    /// Replace the welcome-screen system messages with a single new message.
    pub fn set_system_message(&mut self, msg: ChatMessage) {
        self.system_messages.clear();
        self.system_messages.push(msg);
    }

    /// Refresh the cached modified files list from the focused panel's project mount.
    pub fn refresh_sidebar_modified_files(&mut self) {
        let panel = self.panels.get(self.focused_panel);
        let worktree_path = panel
            .and_then(|p| p.project_mount.as_ref())
            .and_then(|pm| pm.worktree_base.as_ref());

        self.sidebar_modified_files = match worktree_path {
            Some(wt) => {
                let output = std::process::Command::new("git")
                    .args(["status", "--porcelain"])
                    .current_dir(wt)
                    .output();
                match output {
                    Ok(out) => {
                        String::from_utf8_lossy(&out.stdout)
                            .lines()
                            .filter(|l| !l.is_empty())
                            .map(|l| l.to_string())
                            .collect()
                    }
                    Err(_) => Vec::new(),
                }
            }
            None => Vec::new(),
        };
        // Reset scroll if list shrank
        if self.sidebar_files_scroll >= self.sidebar_modified_files.len() {
            self.sidebar_files_scroll = 0;
        }
    }

    /// Refresh the cached committed files list from the focused panel's project mount.
    ///
    /// Runs `git diff --name-only <base_commit>..HEAD` in the clone to find all
    /// files changed by sandbox commits since the branch starting point.
    pub fn refresh_sidebar_committed_files(&mut self) {
        let panel = self.panels.get(self.focused_panel);
        let (worktree_path, base) = match panel {
            Some(p) => {
                let wt = p.project_mount.as_ref().and_then(|pm| pm.worktree_base.as_ref());
                let base = p.base_commit.as_deref();
                (wt, base)
            }
            None => (None, None),
        };

        self.sidebar_committed_files = match (worktree_path, base) {
            (Some(wt), Some(base_sha)) => {
                let range = format!("{}..HEAD", base_sha);
                let output = std::process::Command::new("git")
                    .args(["diff", "--name-only", &range])
                    .current_dir(wt)
                    .output();
                match output {
                    Ok(out) if out.status.success() => {
                        String::from_utf8_lossy(&out.stdout)
                            .lines()
                            .filter(|l| !l.is_empty())
                            .map(|l| l.to_string())
                            .collect()
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        };
        // Reset scroll if list shrank
        if self.sidebar_committed_scroll >= self.sidebar_committed_files.len() {
            self.sidebar_committed_scroll = 0;
        }
    }

    /// Check all panels for new commits in their clones and sync to source repos.
    ///
    /// For each panel with a project mount, compares the clone's HEAD with the
    /// last known SHA. When a new commit is detected, fetches the branch from
    /// the clone back to the source repo so it's immediately visible locally.
    /// Returns a list of (panel_idx, message) pairs for system notifications.
    pub fn sync_project_commits(&mut self) -> Vec<(usize, String)> {
        let mut notifications = Vec::new();
        let global_auto_sync = self.settings.gitsync.auto_sync;
        let notify_on_commit = self.settings.gitsync.notify_on_commit;

        for (panel_idx, panel) in self.panels.iter_mut().enumerate() {
            let pm = match panel.project_mount.as_ref() {
                Some(pm) => pm,
                None => continue,
            };
            let wt_base = match pm.worktree_base.as_ref() {
                Some(wt) => wt,
                None => continue,
            };

            // Get current HEAD SHA in clone.
            let output = match std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(wt_base)
                .output()
            {
                Ok(out) if out.status.success() => out,
                _ => continue,
            };
            let current_head = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if current_head.is_empty() {
                continue;
            }

            // First poll — record HEAD as baseline for committed-files diff.
            if panel.last_known_head.is_none() {
                panel.base_commit = Some(current_head.clone());
                panel.last_known_head = Some(current_head);
                continue;
            }

            // No change — skip.
            if panel.last_known_head.as_deref() == Some(&current_head) {
                continue;
            }

            // Determine if auto-sync is active for this panel.
            let auto_sync = panel.sync_override
                .unwrap_or(global_auto_sync);

            // Get commit info for notification.
            let subject = std::process::Command::new("git")
                .args(["log", "--format=%s", "-1"])
                .current_dir(wt_base)
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            let short_sha = &current_head[..7.min(current_head.len())];

            if auto_sync {
                // Fetch from clone to source (existing behavior).
                let (source_path, branch_name) = match pm.created_branches.first() {
                    Some((src, branch)) => (src.clone(), branch.clone()),
                    None => {
                        // If branches not yet created (deferred setup), just notify.
                        if notify_on_commit {
                            notifications.push((
                                panel_idx,
                                format!("New commit {}: {} (use /gitsync now to sync)", short_sha, subject),
                            ));
                        }
                        panel.last_known_head = Some(current_head);
                        continue;
                    }
                };

                let refspec = format!("{}:{}", branch_name, branch_name);
                let fetch_ok = std::process::Command::new("git")
                    .args(["fetch", &wt_base.to_string_lossy(), &refspec, "--force"])
                    .current_dir(&source_path)
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);

                if fetch_ok {
                    notifications.push((
                        panel_idx,
                        format!("Synced {} to {}: {}", short_sha, branch_name, subject),
                    ));
                }
            } else if notify_on_commit {
                // Notify only — don't fetch.
                notifications.push((
                    panel_idx,
                    format!("New commit {}: {} (use /gitsync now to sync)", short_sha, subject),
                ));
            }

            panel.last_known_head = Some(current_head);
        }

        notifications
    }

    /// Switch input focus to the global command bar.
    pub fn focus_global(&mut self) {
        self.input_focus = InputFocus::Global;
        self.autocomplete_index = None;
    }

    /// Switch input focus to the focused panel's input bar.
    pub fn focus_panel_input(&mut self) {
        if !self.panels.is_empty() {
            self.input_focus = InputFocus::Panel;
            self.autocomplete_index = None;
        }
    }

    /// Move focus to the next panel.
    pub fn focus_next(&mut self) {
        if !self.panels.is_empty() {
            self.focused_panel = (self.focused_panel + 1) % self.panels.len();
            self.input_focus = InputFocus::Panel;
            if self.show_sandbox_sidebar {
                self.refresh_sidebar_modified_files();
            }
        }
    }

    /// Move focus to the previous panel.
    pub fn focus_prev(&mut self) {
        if !self.panels.is_empty() {
            self.focused_panel = if self.focused_panel == 0 {
                self.panels.len() - 1
            } else {
                self.focused_panel - 1
            };
            self.input_focus = InputFocus::Panel;
            if self.show_sandbox_sidebar {
                self.refresh_sidebar_modified_files();
            }
        }
    }

    /// Get a mutable reference to the focused panel.
    pub fn focused_panel_mut(&mut self) -> Option<&mut AgentPanel> {
        self.panels.get_mut(self.focused_panel)
    }

    /// Get an immutable reference to the focused panel.
    pub fn focused_panel_ref(&self) -> Option<&AgentPanel> {
        self.panels.get(self.focused_panel)
    }

    /// Get the available input width for the currently active input.
    pub fn active_input_width(&self) -> u16 {
        match self.input_focus {
            InputFocus::Global => self.last_global_input_width,
            InputFocus::Panel => self
                .focused_panel_ref()
                .map_or(self.last_global_input_width, |p| p.last_input_width),
        }
    }

    /// Get an immutable reference to the currently active TextInput.
    pub fn active_input(&self) -> &TextInput {
        match self.input_focus {
            InputFocus::Global => &self.global_input,
            InputFocus::Panel => {
                if let Some(panel) = self.focused_panel_ref() {
                    &panel.input
                } else {
                    &self.global_input
                }
            }
        }
    }

    /// Whether the autocomplete popup is currently showing.
    pub fn autocomplete_active(&self) -> bool {
        self.current_input().starts_with('/')
    }

    /// Append a character to the active input buffer.
    pub fn handle_char(&mut self, c: char) {
        self.autocomplete_index = None;
        match self.input_focus {
            InputFocus::Global => {
                self.global_input.insert_char(c);
            }
            InputFocus::Panel => {
                if let Some(panel) = self.focused_panel_mut() {
                    panel.input.insert_char(c);
                } else {
                    self.global_input.insert_char(c);
                }
            }
        }
    }

    /// Delete the character before the cursor in the active input.
    pub fn handle_backspace(&mut self) {
        self.autocomplete_index = None;
        match self.input_focus {
            InputFocus::Global => {
                self.global_input.backspace();
            }
            InputFocus::Panel => {
                if let Some(panel) = self.focused_panel_mut() {
                    panel.input.backspace();
                } else {
                    self.global_input.backspace();
                }
            }
        }
    }

    /// Delete the character at the cursor in the active input.
    pub fn handle_delete(&mut self) {
        self.autocomplete_index = None;
        match self.input_focus {
            InputFocus::Global => {
                self.global_input.delete();
            }
            InputFocus::Panel => {
                if let Some(panel) = self.focused_panel_mut() {
                    panel.input.delete();
                } else {
                    self.global_input.delete();
                }
            }
        }
    }

    /// Insert a newline at the cursor in the active input.
    pub fn handle_newline(&mut self) {
        self.autocomplete_index = None;
        match self.input_focus {
            InputFocus::Global => {
                self.global_input.insert_newline();
            }
            InputFocus::Panel => {
                if let Some(panel) = self.focused_panel_mut() {
                    panel.input.insert_newline();
                } else {
                    self.global_input.insert_newline();
                }
            }
        }
    }

    /// Move the cursor left in the active input.
    pub fn handle_move_left(&mut self) {
        match self.input_focus {
            InputFocus::Global => self.global_input.move_left(),
            InputFocus::Panel => {
                if let Some(panel) = self.focused_panel_mut() {
                    panel.input.move_left();
                }
            }
        }
    }

    /// Move the cursor right in the active input.
    pub fn handle_move_right(&mut self) {
        match self.input_focus {
            InputFocus::Global => self.global_input.move_right(),
            InputFocus::Panel => {
                if let Some(panel) = self.focused_panel_mut() {
                    panel.input.move_right();
                }
            }
        }
    }

    /// Move the cursor up one visual line in the active input.
    pub fn handle_move_up(&mut self, width: usize) {
        match self.input_focus {
            InputFocus::Global => self.global_input.move_up(width),
            InputFocus::Panel => {
                if let Some(panel) = self.focused_panel_mut() {
                    panel.input.move_up(width);
                }
            }
        }
    }

    /// Move the cursor down one visual line in the active input.
    pub fn handle_move_down(&mut self, width: usize) {
        match self.input_focus {
            InputFocus::Global => self.global_input.move_down(width),
            InputFocus::Panel => {
                if let Some(panel) = self.focused_panel_mut() {
                    panel.input.move_down(width);
                }
            }
        }
    }

    /// Move the cursor to the start of the current line.
    pub fn handle_home(&mut self) {
        match self.input_focus {
            InputFocus::Global => self.global_input.move_home(),
            InputFocus::Panel => {
                if let Some(panel) = self.focused_panel_mut() {
                    panel.input.move_home();
                }
            }
        }
    }

    /// Move the cursor to the end of the current line.
    pub fn handle_end(&mut self) {
        match self.input_focus {
            InputFocus::Global => self.global_input.move_end(),
            InputFocus::Panel => {
                if let Some(panel) = self.focused_panel_mut() {
                    panel.input.move_end();
                }
            }
        }
    }

    /// Get the current input text based on current focus.
    pub fn current_input(&self) -> &str {
        self.active_input().text()
    }

    /// Submit the input buffer.
    ///
    /// Returns a [`SubmitResult`] indicating what the user typed:
    /// a regular message, a slash command, empty input, or no panel.
    /// When no panels exist, only slash commands are accepted.
    pub fn handle_submit(&mut self) -> SubmitResult {
        match self.input_focus {
            InputFocus::Global => {
                let input = self.global_input.clear();
                let input = input.trim().to_string();
                if input.is_empty() {
                    return SubmitResult::Empty;
                }
                // Global bar only accepts slash commands.
                match commands::parse_command_verbose(&input) {
                    ParseResult::Ok(cmd) => SubmitResult::Command(cmd),
                    ParseResult::Err(msg) => SubmitResult::CommandError(msg),
                    ParseResult::NotACommand => SubmitResult::CommandError(
                        "Global bar only accepts /commands. Type in a panel to send messages."
                            .to_string(),
                    ),
                }
            }
            InputFocus::Panel => {
                let input = if let Some(panel) = self.focused_panel_mut() {
                    let input = panel.input.clear();
                    input.trim().to_string()
                } else {
                    return SubmitResult::NoPanel;
                };

                if input.is_empty() {
                    return SubmitResult::Empty;
                }

                match commands::parse_command_verbose(&input) {
                    ParseResult::Ok(cmd) => return SubmitResult::Command(cmd),
                    ParseResult::Err(msg) => return SubmitResult::CommandError(msg),
                    ParseResult::NotACommand => {}
                }

                // Add to chat history.
                if let Some(panel) = self.focused_panel_mut() {
                    panel.chat_history.push(ChatMessage {
                        role: MessageRole::User,
                        content: input.clone(),
                    });
                }

                SubmitResult::Message(input)
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_initial_state() {
        let app = App::new();
        assert!(app.panels.is_empty());
        assert_eq!(app.focused_panel, 0);
        assert!(!app.should_quit);
        assert!(!app.show_mcp_sidebar);
        assert!(app.show_welcome);
        assert!(app.global_input.is_empty());
        assert!(app.system_messages.is_empty());
        assert!(app.focused_panel_ref().is_none());
    }

    #[test]
    fn test_input_char_appended() {
        let mut app = App::new();
        app.panels.push(AgentPanel::new("test"));
        app.input_focus = InputFocus::Panel;
        app.handle_char('h');
        assert_eq!(app.panels[0].input.text(), "h");
        app.handle_char('i');
        assert_eq!(app.panels[0].input.text(), "hi");
    }

    #[test]
    fn test_input_backspace() {
        let mut app = App::new();
        app.panels.push(AgentPanel::new("test"));
        app.input_focus = InputFocus::Panel;
        app.handle_char('a');
        app.handle_char('b');
        app.handle_backspace();
        assert_eq!(app.panels[0].input.text(), "a");
    }

    #[test]
    fn test_submit_message_adds_to_chat() {
        let mut app = App::new();
        app.panels.push(AgentPanel::new("test"));
        app.input_focus = InputFocus::Panel;
        app.handle_char('h');
        app.handle_char('i');
        let result = app.handle_submit();
        assert!(matches!(result, SubmitResult::Message(ref msg) if msg == "hi"));
        assert!(app.panels[0].input.is_empty());
        assert_eq!(app.panels[0].chat_history.len(), 1);
        assert_eq!(app.panels[0].chat_history[0].role, MessageRole::User);
    }

    #[test]
    fn test_submit_command_returns_command() {
        let mut app = App::new();
        app.panels.push(AgentPanel::new("test"));
        app.input_focus = InputFocus::Panel;
        for c in "/quit".chars() {
            app.handle_char(c);
        }
        let result = app.handle_submit();
        assert!(matches!(result, SubmitResult::Command(_)));
    }

    #[test]
    fn test_submit_empty_returns_empty() {
        let mut app = App::new();
        app.panels.push(AgentPanel::new("test"));
        app.input_focus = InputFocus::Panel;
        let result = app.handle_submit();
        assert!(matches!(result, SubmitResult::Empty));
    }

    #[test]
    fn test_global_input_when_no_panels() {
        let mut app = App::new();
        // With no panels, input goes to global_input
        app.handle_char('/');
        app.handle_char('a');
        app.handle_char('d');
        app.handle_char('d');
        app.handle_char(' ');
        app.handle_char('c');
        assert_eq!(app.global_input.text(), "/add c");
        assert_eq!(app.current_input(), "/add c");

        // Backspace works on global input
        app.handle_backspace();
        assert_eq!(app.global_input.text(), "/add ");

        // Submit parses command from global input
        app.handle_char('c');
        app.handle_char('l');
        app.handle_char('a');
        app.handle_char('u');
        app.handle_char('d');
        app.handle_char('e');
        let result = app.handle_submit();
        assert!(matches!(result, SubmitResult::Command(Command::AddAgent { ref agent, .. }) if agent == "claude"));
        assert!(app.global_input.is_empty());
    }

    #[test]
    fn test_global_input_non_command_rejected() {
        let mut app = App::new();
        app.handle_char('h');
        app.handle_char('i');
        let result = app.handle_submit();
        // Non-commands on global bar are rejected with an error
        assert!(matches!(result, SubmitResult::CommandError(_)));
        assert!(app.global_input.is_empty());
    }

    #[test]
    fn test_panel_default_mode_is_loading() {
        let panel = AgentPanel::new("test");
        assert_eq!(panel.mode, PanelMode::Loading);
    }

    #[test]
    fn test_panel_default_env_is_empty() {
        let panel = AgentPanel::new("test");
        assert!(panel.env.is_empty());
    }

    #[test]
    fn test_global_bar_rejects_non_commands_when_panels_exist() {
        let mut app = App::new();
        app.panels.push(AgentPanel::new("test"));
        // input_focus stays Global (default)
        for c in "hello".chars() {
            app.handle_char(c);
        }
        let result = app.handle_submit();
        assert!(matches!(result, SubmitResult::CommandError(_)));
    }

    #[test]
    fn test_focus_toggle_methods() {
        let mut app = App::new();
        app.panels.push(AgentPanel::new("test"));
        assert_eq!(app.input_focus, InputFocus::Global);

        app.focus_panel_input();
        assert_eq!(app.input_focus, InputFocus::Panel);

        app.focus_global();
        assert_eq!(app.input_focus, InputFocus::Global);
    }

    #[test]
    fn test_focus_next_sets_panel_input() {
        let mut app = App::new();
        app.panels.push(AgentPanel::new("a"));
        app.panels.push(AgentPanel::new("b"));
        assert_eq!(app.input_focus, InputFocus::Global);
        app.focus_next();
        assert_eq!(app.input_focus, InputFocus::Panel);
        assert_eq!(app.focused_panel, 1);
    }

    #[test]
    fn test_cursor_movement_in_panel() {
        let mut app = App::new();
        app.panels.push(AgentPanel::new("test"));
        app.input_focus = InputFocus::Panel;
        app.handle_char('a');
        app.handle_char('b');
        app.handle_char('c');
        app.handle_move_left();
        assert_eq!(app.panels[0].input.cursor(), 2);
        app.handle_char('X');
        assert_eq!(app.panels[0].input.text(), "abXc");
    }

    #[test]
    fn test_zoomed_default_false() {
        let app = App::new();
        assert!(!app.zoomed);
    }

    #[test]
    fn test_toggle_zoom() {
        let mut app = App::new();
        app.panels.push(AgentPanel::new("test"));
        app.zoomed = !app.zoomed;
        assert!(app.zoomed);
        app.zoomed = !app.zoomed;
        assert!(!app.zoomed);
    }
}
