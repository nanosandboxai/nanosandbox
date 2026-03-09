//! Application state for the TUI.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::Sandbox;

use super::commands::{self, Command, ParseResult};
use super::terminal::{SshTerminal, SshTerminalHandle};
use super::text_input::TextInput;

/// Operating mode for a panel's input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelMode {
    /// Messages are sent to the agent via send_message.
    Agent,
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
    /// Display name of the agent.
    pub agent_name: String,
    /// The sandbox instance backing this agent, wrapped in Arc<Mutex<>> for
    /// shared access between the event loop and background streaming tasks.
    pub sandbox: Option<Arc<Mutex<Sandbox>>>,
    /// Chat history for this panel.
    pub chat_history: Vec<ChatMessage>,
    /// Current input buffer with cursor tracking and multiline support.
    pub input: TextInput,
    /// Vertical scroll offset for the chat view.
    pub scroll_offset: u16,
    /// Whether the agent is currently streaming output.
    pub is_streaming: bool,
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
    /// URLs already opened in the host browser (dedup within session).
    pub opened_urls: HashSet<String>,
    /// Trailing bytes from the last TerminalData chunk for cross-chunk URL detection.
    pub terminal_url_buffer: Vec<u8>,
}

impl AgentPanel {
    /// Create a new agent panel with the given name.
    pub fn new(agent_name: &str) -> Self {
        Self {
            agent_name: agent_name.to_string(),
            sandbox: None,
            chat_history: Vec::new(),
            input: TextInput::new(),
            scroll_offset: 0,
            is_streaming: false,
            sandbox_id_short: String::new(),
            env: HashMap::new(),
            mode: PanelMode::Agent,
            last_input_width: 40,
            terminal: None,
            terminal_handle: None,
            last_terminal_size: (80, 24),
            opened_urls: HashSet::new(),
            terminal_url_buffer: Vec::new(),
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
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// Create a new application with default state.
    pub fn new() -> Self {
        Self {
            panels: Vec::new(),
            focused_panel: 0,
            should_quit: false,
            show_mcp_sidebar: false,
            show_sandbox_sidebar: false,
            show_welcome: true,
            global_input: TextInput::new(),
            system_messages: Vec::new(),
            autocomplete_index: None,
            input_focus: InputFocus::Global,
            last_global_input_width: 40,
        }
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

    /// Append output text from an agent to the given panel.
    ///
    /// If the last message in the panel is already from the agent,
    /// the text is appended to it (streaming). Otherwise a new agent
    /// message is created.
    pub fn append_agent_output(&mut self, panel_idx: usize, text: &str, _is_stderr: bool) {
        if let Some(panel) = self.panels.get_mut(panel_idx) {
            if let Some(last) = panel.chat_history.last_mut() {
                if last.role == MessageRole::Agent {
                    last.content.push_str(text);
                    return;
                }
            }
            panel.chat_history.push(ChatMessage {
                role: MessageRole::Agent,
                content: text.to_string(),
            });
        }
    }

    /// Mark an agent as finished streaming.
    pub fn mark_agent_done(&mut self, panel_idx: usize, _exit_code: i32) {
        if let Some(panel) = self.panels.get_mut(panel_idx) {
            panel.is_streaming = false;
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
    fn test_panel_default_mode_is_agent() {
        let panel = AgentPanel::new("test");
        assert_eq!(panel.mode, PanelMode::Agent);
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
}
