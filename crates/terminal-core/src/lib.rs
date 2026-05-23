//! Embedded SSH terminal for TUI panels.
//!
//! Manages SSH connections to sandbox VMs, maintains terminal state via vt100,
//! and provides handles for sending keystrokes and resize events.

use std::collections::HashMap;
use std::path::PathBuf;

use tokio::sync::mpsc;

/// Terminal stream events emitted by SSH sessions.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TerminalEvent {
    TerminalData { data: Vec<u8> },
    SshDisconnected { error: Option<String> },
}

/// Key identifiers independent from any frontend/event library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Tab,
    Esc,
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Insert,
    F(u8),
    Unknown,
}

/// Keyboard modifier flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

/// Terminal state backed by a vt100 parser.
///
/// Holds the virtual terminal screen buffer that is rendered by
/// `tui_term::widget::PseudoTerminal` in the renderer.
pub struct SshTerminal {
    /// vt100 parser maintaining the terminal screen state.
    parser: vt100::Parser,
    /// Current terminal dimensions (cols, rows).
    size: (u16, u16),
    /// Current scrollback offset (0 = live screen, >0 = lines scrolled up).
    scroll_offset: usize,
    /// Whether to auto-scroll to bottom when new data arrives.
    auto_scroll: bool,
}

impl SshTerminal {
    /// Create a new terminal with the given dimensions.
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, 1000),
            size: (cols, rows),
            scroll_offset: 0,
            auto_scroll: true,
        }
    }

    /// Get the current screen state for rendering.
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Feed raw bytes from the SSH channel into the terminal parser.
    pub fn process_bytes(&mut self, data: &[u8]) {
        self.parser.process(data);
        // Keep the view pinned to the live screen when auto-scroll is on.
        if self.auto_scroll {
            self.scroll_offset = 0;
            self.parser.set_scrollback(0);
        } else {
            // vt100 auto-increments scrollback_offset internally as new lines
            // push content into the scrollback buffer (grid.rs scroll logic),
            // but only caps to scrollback.len() — not to rows_len. If the
            // offset exceeds rows_len, visible_rows() underflows and panics:
            //   rows_len - scrollback_offset (usize subtraction overflow)
            // Re-sync our tracked value from vt100 and clamp to rows_len.
            let rows = self.size.1 as usize;
            self.scroll_offset = self.parser.screen().scrollback().min(rows);
            self.parser.set_scrollback(self.scroll_offset);
        }
    }

    /// Resize the terminal. Updates the vt100 parser dimensions.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        if (cols, rows) != self.size {
            // Reset scrollback before resize — content may not align after.
            if self.scroll_offset > 0 {
                self.scroll_offset = 0;
                self.parser.set_scrollback(0);
                self.auto_scroll = true;
            }
            self.size = (cols, rows);
            self.parser.set_size(rows, cols);
        }
    }

    /// Scroll up by N lines. Returns true if scroll position changed.
    pub fn scroll_up(&mut self, lines: usize) -> bool {
        let old = self.scroll_offset;
        // The vt100 crate's visible_rows() computes `rows.len() - scrollback_offset`,
        // so the offset must not exceed the terminal height. We also cap to the
        // actual scrollback buffer length.
        let rows = self.size.1 as usize;
        self.parser.set_scrollback(rows);
        let max = self.parser.screen().scrollback(); // clamped to min(rows, scrollback.len())
        self.scroll_offset = (old + lines).min(max);
        self.parser.set_scrollback(self.scroll_offset);
        self.auto_scroll = false;
        self.scroll_offset != old
    }

    /// Scroll down by N lines. Returns true if scroll position changed.
    pub fn scroll_down(&mut self, lines: usize) -> bool {
        let old = self.scroll_offset;
        self.scroll_offset = old.saturating_sub(lines);
        self.parser.set_scrollback(self.scroll_offset);
        if self.scroll_offset == 0 {
            self.auto_scroll = true;
        }
        self.scroll_offset != old
    }

    /// Jump to bottom (live screen).
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.parser.set_scrollback(0);
        self.auto_scroll = true;
    }

    /// Check if currently scrolled up (not at live screen).
    pub fn is_scrolled_up(&self) -> bool {
        self.scroll_offset > 0
    }

    /// Get the current scroll offset.
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Return the maximum scrollback lines currently available.
    pub fn scrollback_max(&mut self) -> usize {
        let rows = self.size.1 as usize;
        let saved = self.scroll_offset;
        self.parser.set_scrollback(rows);
        let max = self.parser.screen().scrollback();
        self.parser.set_scrollback(saved);
        max
    }
}

/// Handle for interacting with an active SSH terminal session.
///
/// Stored in `AgentPanel` — used to send keystrokes and resize events
/// to the background SSH task.
pub struct SshTerminalHandle {
    /// Send raw bytes (keystrokes) to the SSH channel.
    pub write_tx: mpsc::UnboundedSender<Vec<u8>>,
    /// Send resize notifications (cols, rows) to the SSH channel.
    pub resize_tx: mpsc::UnboundedSender<(u16, u16)>,
}


/// Minimal russh client handler.
///
/// Accepts all server keys since we only connect to localhost sandboxes
/// with ephemeral keys.
struct SshHandler;

impl russh::client::Handler for SshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// Connect to a sandbox via SSH and set up an interactive PTY session.
///
/// This:
/// 1. Connects to `127.0.0.1:{ssh_port}`
/// 2. Authenticates with the ed25519 private key
/// 3. Opens a session channel with a PTY
/// 4. Exports environment variables and auto-launches the agent CLI
/// 5. Spawns background tasks for reading/writing the SSH channel
///
/// Returns an `SshTerminalHandle` for the caller to send keystrokes and resize events.
#[allow(clippy::too_many_arguments)]
pub async fn connect_ssh(
    ssh_host: String,
    ssh_port: u16,
    key_path: PathBuf,
    cols: u16,
    rows: u16,
    agent_name: &str,
    env: &HashMap<String, String>,
    workdir: Option<&str>,
    permissions: sandbox::Permissions,
    auto_mode: bool,
    _prompt: Option<&str>,
    is_resumed: bool,
    had_interaction: bool,
    selected_session_id: Option<&str>,
    model: Option<&str>,
    panel_idx: usize,
    tx: mpsc::UnboundedSender<TerminalEvent>,
) -> Result<SshTerminalHandle, anyhow::Error> {
    // Load the private key
    let key_data = tokio::fs::read_to_string(&key_path).await?;
    let key_pair = russh::keys::decode_secret_key(&key_data, None)?;
    let key_with_hash = russh::keys::PrivateKeyWithHashAlg::new(
        std::sync::Arc::new(key_pair),
        None, // hash_alg is ignored for ed25519
    );

    // Legacy Windows localhost SSH used an HvSocket-backed path that could
    // stall. We still detect that case and use more conservative settings.
    let is_localhost_ssh = ssh_host == "127.0.0.1" || ssh_host.eq_ignore_ascii_case("localhost");

    let aggressive_stall_detection = cfg!(target_os = "windows") && is_localhost_ssh;
    let read_timeout = if aggressive_stall_detection {
        std::time::Duration::from_secs(30)
    } else {
        std::time::Duration::from_secs(3600)
    };
    tracing::info!(
        panel_idx,
        ssh_host = %ssh_host,
        ssh_port,
        is_localhost_ssh,
        aggressive_stall_detection,
        read_timeout_secs = read_timeout.as_secs(),
        "SSH terminal transport profile"
    );

    // Connect
    let config = {
        #[cfg(target_os = "windows")]
        let mut config = russh::client::Config::default();
        #[cfg(not(target_os = "windows"))]
        let config = russh::client::Config::default();

    // Windows: SSH keepalive to detect dead connections.
        #[cfg(target_os = "windows")]
        {
            config.keepalive_interval = Some(std::time::Duration::from_secs(15));
            config.keepalive_max = 3;
        }

    // Legacy localhost/HvSocket path: cap SSH window to avoid transport
    // backpressure deadlocks. For TCP guest_ip path, keep default values.
        #[cfg(target_os = "windows")]
        {
            if is_localhost_ssh {
                config.window_size = 262144; // 256KB
                config.maximum_packet_size = 32768;
            }
        }

        config
    };

    let config = std::sync::Arc::new(config);
    let sh = SshHandler;
    let mut session =
        russh::client::connect(config, format!("{}:{}", ssh_host, ssh_port), sh).await?;

    // Authenticate
    let auth_result = session
        .authenticate_publickey("developer", key_with_hash)
        .await?;
    if !matches!(auth_result, russh::client::AuthResult::Success) {
        anyhow::bail!("SSH public key authentication failed");
    }

    // Build the initialization commands (cd + agent CLI launch).
    // All env vars (secrets + config + agent-specific) are injected by the
    // gateway's embedded SSH server via cmd.Env — no shell export needed.
    let mut env_parts: Vec<String> = Vec::new();

    if let Some(dir) = workdir {
        env_parts.push(format!("cd '{}'", dir));
    }

    // Check whether a provider is available for the agent. For goose this
    // determines whether we launch `goose session` or `goose configure`.
    let has_provider = agent_name != "goose"
        || env.contains_key("GOOSE_PROVIDER")
        || env.contains_key("ANTHROPIC_API_KEY")
        || env.contains_key("OPENAI_API_KEY")
        || had_interaction;

    let agent_cmd = agent_cli_command_with_session(
        agent_name,
        permissions,
        auto_mode,
        is_resumed,
        selected_session_id,
        model,
        has_provider,
    );

    let channel = session.channel_open_session().await?;

    if auto_mode {
        // Headless: use channel.exec() to run a single compound command directly.
        // This avoids shell stdin buffering issues and PTY requirements.
        //
        // Wrap the agent command with `script -qfc` to allocate a pseudo-TTY.
        // Without a TTY, Node.js/Rust CLIs (Claude Code, Codex) default to
        // full stdout buffering (~4-8KB), causing NDJSON lines to never flush.
        // `script -qfc "cmd" /dev/null` forces line-buffered output via a PTY.
        // Our ANSI stripper in parse_headless_data handles any escape sequences.
        if let Some(cmd) = &agent_cmd {
            let escaped_cmd = cmd.replace('"', "\\\"");
            env_parts.push(format!("exec script -qfc \"{}\" /dev/null", escaped_cmd));
        }
        let compound = env_parts.join(" && ");
        channel.exec(true, compound.as_bytes()).await?;
    } else {
        // Interactive: allocate PTY + shell, send init commands via stdin.
        channel
            .request_pty(
                false,
                "xterm-256color",
                cols as u32,
                rows as u32,
                0,
                0,
                &[],
            )
            .await?;
        channel.request_shell(false).await?;
    }

    // Split the channel into read and write halves
    let (mut read_half, write_half) = channel.split();

    // Create channels for keystroke and resize forwarding
    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (resize_tx, mut resize_rx) = mpsc::unbounded_channel::<(u16, u16)>();

    if !auto_mode {
        // Interactive mode: send init commands to the shell via stdin
        let mut init_commands = String::new();
        for part in &env_parts {
            init_commands.push_str(&format!("{}\n", part));
        }
        if let Some(cmd) = &agent_cmd {
            init_commands.push_str(&format!("{}\n", cmd));
        }

        use tokio::io::AsyncWriteExt;
        let mut writer = write_half.make_writer();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;

            // Drain any resize events queued during Loading mode so the
            // remote PTY is at the correct size before the agent CLI starts.
            while let Ok((cols, rows)) = resize_rx.try_recv() {
                let _ = write_half.window_change(cols as u32, rows as u32, 0, 0).await;
            }

            let _ = writer.write_all(init_commands.as_bytes()).await;
            let _ = writer.flush().await;

            // Then enter the write loop: forward keystrokes + handle resize
            loop {
                tokio::select! {
                    Some(data) = write_rx.recv() => {
                        if writer.write_all(&data).await.is_err() {
                            break;
                        }
                        // Flush immediately — without this, small writes (single
                        // keystrokes) can get stuck in russh's internal buffer
                        // and the remote shell never sees them, causing the
                        // session to appear frozen.
                        if writer.flush().await.is_err() {
                            break;
                        }
                    }
                    Some((cols, rows)) = resize_rx.recv() => {
                        let _ = write_half.window_change(cols as u32, rows as u32, 0, 0).await;
                    }
                    else => break,
                }
            }
        });
    } else {
        // Headless mode: no stdin needed (command runs via exec).
        // Keep the channels alive but don't spawn a write loop.
        tokio::spawn(async move {
            let _write_half = write_half;
            let _write_rx = write_rx;
            let _resize_rx = resize_rx;
            // Hold references to prevent channel close until read loop ends.
            tokio::time::sleep(std::time::Duration::from_secs(86400)).await;
        });
    }

    // Spawn read loop: forwards SSH channel data to TUI events.
    let tx_read = tx;
    tokio::spawn(async move {
        use russh::ChannelMsg;

        let mut consecutive_timeouts: u32 = 0;

        loop {
            let msg = tokio::time::timeout(read_timeout, read_half.wait()).await;
            match msg {
                Ok(Some(msg)) => {
                    consecutive_timeouts = 0;
                    match msg {
                        ChannelMsg::Data { data } => {
                            let _ = tx_read.send(TerminalEvent::TerminalData { data: data.to_vec() });
                        }
                        ChannelMsg::ExtendedData { data, .. } => {
                            let _ = tx_read.send(TerminalEvent::TerminalData { data: data.to_vec() });
                        }
                        ChannelMsg::Eof | ChannelMsg::Close => {
                            let _ = tx_read.send(TerminalEvent::SshDisconnected { error: None });
                            break;
                        }
                        ChannelMsg::ExitStatus { exit_status } => {
                            let _ = tx_read.send(TerminalEvent::SshDisconnected {
                                error: if exit_status != 0 {
                                    Some(format!("Remote process exited with status {}", exit_status))
                                } else {
                                    None
                                },
                            });
                        }
                        _ => {}
                    }
                }
                Ok(None) => {
                    // Channel dropped / connection lost.
                    tracing::warn!(panel_idx, "SSH channel read returned None (connection lost)");
                    let _ = tx_read.send(TerminalEvent::SshDisconnected {
                        error: Some("SSH connection lost".to_string()),
                    });
                    break;
                }
                Err(_elapsed) => {
                    consecutive_timeouts += 1;
                    tracing::warn!(
                        panel_idx,
                        consecutive_timeouts,
                        timeout_secs = read_timeout.as_secs(),
                        "SSH channel read timed out"
                    );
                    let max_timeouts: u32 = if aggressive_stall_detection { 1 } else { 3 };
                    if consecutive_timeouts >= max_timeouts {
                        let msg = if aggressive_stall_detection {
                            "SSH connection stalled (no data received). Use /reconnect to retry.".to_string()
                        } else {
                            "SSH channel timed out waiting for data.".to_string()
                        };
                        let _ = tx_read.send(TerminalEvent::SshDisconnected {
                            error: Some(msg),
                        });
                        break;
                    }
                }
            }
        }
    });

    Ok(SshTerminalHandle {
        write_tx,
        resize_tx,
    })
}

impl SshTerminalHandle {
    /// Create a no-op handle for headless gateway exec (no SSH session).
    pub fn noop() -> Self {
        let (write_tx, _) = mpsc::unbounded_channel();
        let (resize_tx, _) = mpsc::unbounded_channel();
        Self { write_tx, resize_tx }
    }
}

/// Build agent-specific environment variables (e.g. Goose mode, prompt).
///
/// Returns a map of extra env vars that both the SSH and gateway exec paths
/// need to set for the agent to work correctly.
pub fn agent_env_vars(
    agent_name: &str,
    permissions: sandbox::Permissions,
    auto_mode: bool,
    model: Option<&str>,
    prompt: Option<&str>,
) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    let effective = permissions.effective(auto_mode);

    // Goose permissions via GOOSE_MODE env var.
    if agent_name == "goose" {
        let goose_mode = match effective {
            sandbox::Permissions::AllowAll => "auto",
            sandbox::Permissions::AcceptEdits => "smart_approve",
            sandbox::Permissions::Default => "smart_approve",
        };
        vars.insert("GOOSE_MODE".to_string(), goose_mode.to_string());
        if let Some(m) = model {
            vars.insert("GOOSE_MODEL".to_string(), m.to_string());
            vars.insert("GOOSE_DEFAULT_MODEL".to_string(), m.to_string());
        }
    }

    // Prompt env var for headless agents.
    if auto_mode {
        if let Some(p) = prompt {
            vars.insert("NANOSB_PROMPT".to_string(), p.to_string());
        }
    }

    vars
}

/// Map agent name to the CLI command to auto-launch after SSH connection.
///
/// When `auto_mode` is true (headless), the agent is launched in non-interactive
/// print mode with stream-json output. The prompt is supplied via the
/// `$NANOSB_PROMPT` environment variable (set by `connect_ssh`).
///
/// `permissions` controls the agent's approval level independently of headless mode.
/// When `auto_mode` is true, permissions are always forced to `AllowAll`.
///
/// When `is_resumed` is true, uses the agent's session resume command so it
/// picks up previous conversation context from `/workspace/.nanosb-state/`.
pub fn agent_cli_command(
    agent_name: &str,
    permissions: sandbox::Permissions,
    auto_mode: bool,
    is_resumed: bool,
    model: Option<&str>,
) -> Option<String> {
    agent_cli_command_with_session(agent_name, permissions, auto_mode, is_resumed, None, model, true)
}

pub fn agent_cli_command_with_session(
    agent_name: &str,
    permissions: sandbox::Permissions,
    auto_mode: bool,
    is_resumed: bool,
    selected_session_id: Option<&str>,
    model: Option<&str>,
    has_provider: bool,
) -> Option<String> {
    use sandbox::Permissions;
    let effective = permissions.effective(auto_mode);

    match agent_name {
        "claude" | "claude-code" => {
            let mut parts = vec!["claude".to_string()];

            if auto_mode {
                // Headless: -p mode with stream-json for structured output
                parts.extend([
                    "-p".to_string(),
                    "\"$NANOSB_PROMPT\"".to_string(),
                    "--output-format".to_string(),
                    "stream-json".to_string(),
                    "--verbose".to_string(),
                ]);
                if let Some(session_id) = selected_session_id {
                    parts.extend(["--resume".to_string(), session_id.to_string()]);
                } else if is_resumed {
                    parts.push("--continue".to_string());
                }
            } else if let Some(session_id) = selected_session_id {
                parts.extend(["--resume".to_string(), session_id.to_string()]);
            } else if is_resumed {
                parts.push("-c".to_string());
            }

            match effective {
                Permissions::AllowAll => {
                    parts.push("--dangerously-skip-permissions".to_string());
                    // Load gateway-generated MCP config and CLAUDE.md instructions.
                    // --mcp-config loads MCP servers from ~/.claude.json (user scope).
                    // --add-dir loads CLAUDE.md from ~/.claude/ (gateway-written instructions).
                    parts.extend([
                        "--mcp-config".to_string(),
                        "$HOME/.claude.json".to_string(),
                        "--add-dir".to_string(),
                        "$HOME/.claude".to_string(),
                    ]);
                }
                Permissions::AcceptEdits => {
                    parts.extend([
                        "--permission-mode".to_string(),
                        "acceptEdits".to_string(),
                    ]);
                }
                Permissions::Default => {}
            }

            if let Some(m) = model {
                parts.extend(["--model".to_string(), m.to_string()]);
            }

            Some(parts.join(" "))
        }
        "goose" => {
            // Goose permissions and model handled via env vars in connect_ssh.
            if auto_mode {
                Some("goose run --output-format stream-json --no-session -t \"$NANOSB_PROMPT\"".to_string())
            } else if let Some(session_id) = selected_session_id {
                Some(format!("goose session --resume --session-id {}", session_id))
            } else if is_resumed {
                Some("goose session -r".to_string())
            } else if !has_provider {
                // No provider configured and no existing session — launch the
                // interactive configuration wizard so the user can set up their
                // provider/model before starting a session.
                Some("goose configure".to_string())
            } else {
                Some("goose session".to_string())
            }
        }
        "codex" => {
            if auto_mode {
                let mut parts = vec![
                    "codex".to_string(),
                    "exec".to_string(),
                    "--json".to_string(),
                ];
                match effective {
                    Permissions::AllowAll => parts.push("--yolo".to_string()),
                    _ => parts.push("--full-auto".to_string()),
                }
                if let Some(m) = model {
                    parts.extend(["--model".to_string(), m.to_string()]);
                }
                parts.push("\"$NANOSB_PROMPT\"".to_string());
                Some(parts.join(" "))
            } else {
                let mut parts = vec!["codex".to_string()];
                if let Some(session_id) = selected_session_id {
                    parts.extend(["resume".to_string(), session_id.to_string()]);
                } else if is_resumed {
                    parts.extend(["resume".to_string(), "--last".to_string()]);
                }
                match effective {
                    Permissions::AllowAll => parts.push("--yolo".to_string()),
                    Permissions::AcceptEdits => parts.push("--full-auto".to_string()),
                    Permissions::Default => {}
                }
                if let Some(m) = model {
                    parts.extend(["--model".to_string(), m.to_string()]);
                }
                Some(parts.join(" "))
            }
        }
        "cursor" | "cursor-agent" => {
            let mut parts = vec!["cursor-agent".to_string()];

            if auto_mode {
                parts.extend([
                    "-p".to_string(),
                    "\"$NANOSB_PROMPT\"".to_string(),
                    "--output-format".to_string(),
                    "stream-json".to_string(),
                ]);
                if let Some(session_id) = selected_session_id {
                    parts.extend(["--resume".to_string(), session_id.to_string()]);
                } else if is_resumed {
                    parts.push("--continue".to_string());
                }
            } else if let Some(session_id) = selected_session_id {
                parts.extend(["--resume".to_string(), session_id.to_string()]);
            } else if is_resumed {
                parts.push("--continue".to_string());
            }

            match effective {
                Permissions::AllowAll => {
                    parts.push("--force".to_string());
                    // --trust and --approve-mcps require --print (headless mode)
                    if auto_mode {
                        parts.extend([
                            "--trust".to_string(),
                            "--approve-mcps".to_string(),
                        ]);
                    }
                }
                Permissions::AcceptEdits => {
                    // --trust requires --print (headless mode)
                    if auto_mode {
                        parts.push("--trust".to_string());
                    }
                }
                Permissions::Default => {}
            }

            if let Some(m) = model {
                parts.extend(["--model".to_string(), m.to_string()]);
            }

            Some(parts.join(" "))
        }
        _ => None,
    }
}

/// Convert a neutral key + modifiers into bytes expected by a remote PTY.
pub fn key_to_bytes(key: Key, modifiers: Modifiers) -> Vec<u8> {
    match key {
        Key::Char(c) if modifiers.ctrl => {
            // Ctrl+A..Z = 0x01..0x1A
            let byte = (c.to_ascii_lowercase() as u8).wrapping_sub(b'a').wrapping_add(1);
            vec![byte]
        }
        Key::Char(c) if modifiers.alt => {
            // Alt+<key> = ESC followed by the key
            let mut buf = vec![0x1b];
            let mut char_buf = [0u8; 4];
            let s = c.encode_utf8(&mut char_buf);
            buf.extend_from_slice(s.as_bytes());
            buf
        }
        Key::Char(c) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            s.as_bytes().to_vec()
        }
        Key::Enter => vec![b'\r'],
        Key::Backspace => vec![0x7f],
        Key::Tab => vec![b'\t'],
        Key::Esc => vec![0x1b],
        Key::Up => b"\x1b[A".to_vec(),
        Key::Down => b"\x1b[B".to_vec(),
        Key::Right => b"\x1b[C".to_vec(),
        Key::Left => b"\x1b[D".to_vec(),
        Key::Home => b"\x1b[H".to_vec(),
        Key::End => b"\x1b[F".to_vec(),
        Key::PageUp => b"\x1b[5~".to_vec(),
        Key::PageDown => b"\x1b[6~".to_vec(),
        Key::Delete => b"\x1b[3~".to_vec(),
        Key::Insert => b"\x1b[2~".to_vec(),
        Key::F(1) => b"\x1bOP".to_vec(),
        Key::F(2) => b"\x1bOQ".to_vec(),
        Key::F(3) => b"\x1bOR".to_vec(),
        Key::F(4) => b"\x1bOS".to_vec(),
        Key::F(5) => b"\x1b[15~".to_vec(),
        Key::F(6) => b"\x1b[17~".to_vec(),
        Key::F(7) => b"\x1b[18~".to_vec(),
        Key::F(8) => b"\x1b[19~".to_vec(),
        Key::F(9) => b"\x1b[20~".to_vec(),
        Key::F(10) => b"\x1b[21~".to_vec(),
        Key::F(11) => b"\x1b[23~".to_vec(),
        Key::F(12) => b"\x1b[24~".to_vec(),
        _ => vec![],
    }
}

/// Maximum size of the cross-chunk URL buffer (bytes kept from previous chunk).
/// Must be large enough to cover a full OAuth URL (~500+ bytes) so that cross-chunk
/// joins have enough context to detect nested URLs in query parameters.
const URL_BUFFER_SIZE: usize = 2048;

/// Extract https:// URLs from raw terminal data.
///
/// `prev_buffer` contains trailing bytes from the previous chunk to handle URLs
/// that span chunk boundaries. Returns the extracted URLs and the new buffer
/// (trailing bytes from this chunk to carry forward).
///
/// PTY line wrapping inserts `\r\n` in the middle of long URLs. This function
/// rejoins such fragments by skipping `\r`/`\n` that appear between URL-valid
/// characters (a single `\r\n` followed by a URL character is a line wrap, while
/// `\r\n\r\n` or `\r\n` followed by a space/non-URL char is a true break).
///
/// URLs that end at data boundaries (end of chunk or `\r\n` at chunk end) are
/// NOT emitted — they are deferred in the buffer so the next chunk can complete
/// them. This prevents opening truncated URLs when PTY wraps split a URL across
/// SSH data chunks.
pub fn extract_urls(data: &[u8], prev_buffer: &[u8]) -> (Vec<String>, Vec<u8>) {
    // Combine previous buffer with new data for cross-chunk detection.
    let mut combined = Vec::with_capacity(prev_buffer.len() + data.len());
    combined.extend_from_slice(prev_buffer);
    combined.extend_from_slice(data);

    let text = String::from_utf8_lossy(&combined);
    let mut urls = Vec::new();

    let chars: Vec<char> = text.chars().collect();
    let mut search_start = 0;
    // Byte position of a deferred (incomplete) URL for the buffer.
    let mut deferred_url_byte_start: Option<usize> = None;

    while search_start < chars.len() {
        // Find "https://" in the remaining characters.
        let remaining: String = chars[search_start..].iter().collect();
        let byte_pos = match remaining.find("https://") {
            Some(p) => p,
            None => break,
        };

        // Convert byte offset in `remaining` to char offset.
        let char_offset = remaining[..byte_pos].chars().count();
        let url_char_start = search_start + char_offset;

        // Skip https:// that appears as a query parameter value (preceded by '=').
        // This prevents opening nested redirect_uri values as standalone URLs.
        if url_char_start > 0 && chars[url_char_start - 1] == '=' {
            search_start = url_char_start + 8; // skip past "https://"
            continue;
        }

        let mut url = String::new();
        let mut i = url_char_start;
        let mut url_complete = true;

        // Collect URL characters, skipping PTY-inserted \r\n line wraps.
        while i < chars.len() {
            let c = chars[i];

            if c == '\r' || c == '\n' {
                // Look ahead past all \r\n characters.
                let mut j = i;
                let mut newline_count = 0;
                while j < chars.len() && (chars[j] == '\r' || chars[j] == '\n') {
                    if chars[j] == '\n' {
                        newline_count += 1;
                    }
                    j += 1;
                }

                if newline_count > 1 {
                    // Double newline (\r\n\r\n) is always a paragraph break.
                    break;
                } else if j >= chars.len() {
                    // Single \r\n at end of data — might be a PTY line wrap, defer.
                    url_complete = false;
                    break;
                } else if is_url_char(chars[j]) {
                    // Single \r\n followed by URL-valid char — PTY line wrap, skip.
                    i = j;
                    continue;
                } else {
                    // Single \r\n followed by non-URL char — end the URL.
                    break;
                }
            }

            if c == '\x1b' || c == '\x07' || c == '"' || c == '\''
                || c == '<' || c == '>' || c == ' ' || c == '\t'
            {
                break;
            }

            url.push(c);
            i += 1;
        }

        // Ran out of data mid-URL — might be incomplete.
        if i >= chars.len() {
            url_complete = false;
        }

        // Strip trailing punctuation that's likely not part of the URL.
        while url.ends_with(['.', ',', ')', ']', ';']) {
            url.pop();
        }

        if url_complete {
            if url.len() > 10 {
                urls.push(url);
            }
        } else {
            // URL might be incomplete — defer by carrying data in the buffer.
            let byte_offset: usize = chars[..url_char_start]
                .iter()
                .map(|c| c.len_utf8())
                .sum();
            deferred_url_byte_start = Some(byte_offset);
            break; // Everything after is also incomplete.
        }

        search_start = i;
    }

    // Compute the new buffer.
    let new_buffer = if let Some(byte_start) = deferred_url_byte_start {
        // Carry the deferred URL data forward for the next call.
        let tail = &combined[byte_start..];
        if tail.len() > URL_BUFFER_SIZE {
            // URL data exceeds buffer — fall back to standard trailing buffer.
            if data.len() >= URL_BUFFER_SIZE {
                data[data.len() - URL_BUFFER_SIZE..].to_vec()
            } else {
                data.to_vec()
            }
        } else {
            tail.to_vec()
        }
    } else {
        // No deferred URL — keep trailing bytes for cross-chunk detection.
        if data.len() >= URL_BUFFER_SIZE {
            data[data.len() - URL_BUFFER_SIZE..].to_vec()
        } else {
            data.to_vec()
        }
    };

    (urls, new_buffer)
}

/// Extract URLs from the parsed vt100 terminal screen.
///
/// Unlike `extract_urls` (which operates on raw SSH bytes), this function reads
/// the vt100 screen contents — clean text with ANSI escape sequences already
/// stripped. This correctly handles URLs rendered by TUI applications (ink,
/// ratatui) where escape sequences for cursor positioning appear between
/// wrapped URL line fragments.
///
/// The screen text is preprocessed: each row is stripped of leading/trailing
/// box-drawing characters and whitespace, then rows are rejoined. The existing
/// `extract_urls` logic handles newline-wrapped URLs from there.
pub fn extract_urls_from_screen(screen: &vt100::Screen) -> Vec<String> {
    let contents = screen.contents();
    // Strip TUI border characters and leading/trailing whitespace per line,
    // then rejoin so extract_urls can handle \n line-wraps.
    let preprocessed: String = contents
        .lines()
        .map(|line| {
            line.trim_matches(|c: char| {
                c.is_whitespace() || ('\u{2500}'..='\u{257F}').contains(&c)
            })
        })
        .collect::<Vec<_>>()
        .join("\n");
    let (urls, _) = extract_urls(preprocessed.as_bytes(), &[]);
    urls
}

/// Known OAuth/authentication domains for AI coding agents.
///
/// Only URLs matching these domains are auto-opened in the host browser.
/// This prevents random URLs printed by agents from spawning browser tabs.
const AUTH_DOMAINS: &[&str] = &[
    // Claude Code
    "claude.com",               // Claude Code OAuth authorize (new host)
    "www.claude.com",           // Claude web redirect/auth host (new host)
    "claude.ai",                // Claude Code CLI (OAuth authorize)
    "www.claude.ai",            // Claude web redirect/auth host
    "platform.claude.com",      // Claude OAuth callback/redirect host
    "auth.anthropic.com",       // Claude Code (token refresh)
    "console.anthropic.com",    // Claude Code (console auth)
    // OpenAI Codex CLI
    "auth.openai.com",          // Codex CLI (OAuth authorize)
    "login.live.com",           // Codex CLI (Microsoft SSO)
    "login.microsoftonline.com",// Codex CLI (Azure AD SSO)
    // Cursor
    "authenticator.cursor.sh",  // Cursor IDE (authenticator)
    "cursor.sh",                // Cursor IDE (legacy domain)
    "cursor.com",               // Cursor IDE (device flow sign-in)
    "www.cursor.com",           // Cursor IDE (www redirect)
    // GitHub (SSO provider for multiple agents)
    "github.com",               // GitHub device flow (Codex, Copilot)
];

/// Check whether a URL is a known OAuth authentication URL.
///
/// Only whitelisted auth domains are allowed to auto-open in the host
/// browser.  This prevents agents from flooding the browser with arbitrary
/// URLs while still supporting OAuth sign-in flows.
pub fn is_auth_url(url: &str) -> bool {
    let after_scheme = match url.strip_prefix("https://") {
        Some(s) => s,
        None => return false,
    };
    let authority = after_scheme
        .split(|c: char| c == '/' || c == '?' || c == '#')
        .next()
        .unwrap_or("");
    // Strip optional port
    let host = authority.split(':').next().unwrap_or(authority);
    AUTH_DOMAINS.iter().any(|d| host.eq_ignore_ascii_case(d))
}

/// Device-code flow path segments.
///
/// Device-code flows (GitHub, Cursor) print a URL without query parameters
/// and display the user code separately.  These URLs are complete as-is and
/// should not be rejected by the `?`-query-param filter.
const DEVICE_CODE_PATHS: &[&str] = &[
    "/login/device",    // GitHub device flow
    "/loginlink",       // Cursor device flow
    "/device",          // Generic device code
];

/// Check whether a URL is a device-code flow URL (no query params needed).
pub fn is_device_code_url(url: &str) -> bool {
    let after_scheme = match url.strip_prefix("https://") {
        Some(s) => s,
        None => return false,
    };
    // Extract path (everything after the host, before ? or #).
    let after_host = match after_scheme.find('/') {
        Some(pos) => &after_scheme[pos..],
        None => return false,
    };
    let path = after_host
        .split(|c: char| c == '?' || c == '#')
        .next()
        .unwrap_or(after_host);
    DEVICE_CODE_PATHS
        .iter()
        .any(|p| path.eq_ignore_ascii_case(p) || path.starts_with(&format!("{}/", p)))
}

/// Extract the `redirect_uri` localhost port from an OAuth URL.
///
/// OAuth flows embed `redirect_uri=http%3A%2F%2Flocalhost%3A<port>%2F...` or
/// `redirect_uri=http://localhost:<port>/...` in the URL.  The callback server
/// runs inside the VM, so we need to forward that port through gvproxy to the
/// host so the browser redirect reaches the VM.
pub fn extract_oauth_callback_port(url: &str) -> Option<u16> {
    // Look for redirect_uri=http...localhost...:<port>
    let redir_start = url.find("redirect_uri=")?;
    let redir_value = &url[redir_start + "redirect_uri=".len()..];

    // URL-decoded: http://localhost:<port>  or  percent-encoded: http%3A%2F%2Flocalhost%3A<port>
    // Normalize by decoding %3A→: and %2F→/
    let decoded = redir_value
        .replace("%3A", ":")
        .replace("%3a", ":")
        .replace("%2F", "/")
        .replace("%2f", "/");

    // Find localhost:<port>
    let after_localhost = decoded
        .find("localhost:")
        .map(|i| &decoded[i + "localhost:".len()..])?;

    // Parse port digits
    let port_str: String = after_localhost
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();

    port_str.parse::<u16>().ok()
}

/// Check if a character is valid within a URL (not a URL terminator).
fn is_url_char(c: char) -> bool {
    // URL-safe characters per RFC 3986 + percent-encoding.
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '-' | '.' | '_' | '~' | ':' | '/' | '?' | '#' | '[' | ']' | '@'
                | '!' | '$' | '&' | '\'' | '(' | ')' | '*' | '+' | ',' | ';'
                | '=' | '%'
        )
}

/// Extract a dedup key from a URL.
///
/// For **auth URLs** (matching [`AUTH_DOMAINS`]) the key is just the host.
/// TUI apps like ink.js re-render via cursor positioning, so partial screen
/// reads may produce truncated paths (e.g. `auth.openai.com/ous` instead of
/// `auth.openai.com/oauth/authorize?…`).  Host-only grouping ensures the
/// "keep longest" debounce correctly replaces truncated URLs with the full one.
///
/// For other URLs the key is scheme + host + path (query string stripped).
pub fn url_dedup_key(url: &str) -> String {
    let after_scheme = match url.strip_prefix("https://") {
        Some(s) => s,
        None => {
            // Non-https: fall back to stripping query string.
            return url
                .find(|c| c == '?' || c == '#')
                .map_or_else(|| url.to_string(), |pos| url[..pos].to_string());
        }
    };

    let host = after_scheme
        .split(|c: char| c == '/' || c == '?' || c == '#' || c == ':')
        .next()
        .unwrap_or(after_scheme);

    // Auth URLs: dedup by host only so truncated paths still match.
    if AUTH_DOMAINS.iter().any(|d| host.eq_ignore_ascii_case(d)) {
        return format!("https://{}", host);
    }

    // Non-auth: keep scheme + host + path, strip query/fragment.
    if let Some(pos) = url.find(|c| c == '?' || c == '#') {
        url[..pos].to_string()
    } else {
        url.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_to_bytes_char() {
        assert_eq!(key_to_bytes(Key::Char('a'), Modifiers::default()), b"a");
    }

    #[test]
    fn test_key_to_bytes_enter() {
        assert_eq!(key_to_bytes(Key::Enter, Modifiers::default()), b"\r");
    }

    #[test]
    fn test_key_to_bytes_ctrl_c() {
        assert_eq!(
            key_to_bytes(
                Key::Char('c'),
                Modifiers {
                    ctrl: true,
                    ..Default::default()
                },
            ),
            vec![0x03],
        );
    }

    #[test]
    fn test_key_to_bytes_arrow_up() {
        assert_eq!(key_to_bytes(Key::Up, Modifiers::default()), b"\x1b[A");
    }

    #[test]
    fn test_key_to_bytes_backspace() {
        assert_eq!(key_to_bytes(Key::Backspace, Modifiers::default()), vec![0x7f]);
    }

    #[test]
    fn test_key_to_bytes_alt_char() {
        assert_eq!(
            key_to_bytes(
                Key::Char('x'),
                Modifiers {
                    alt: true,
                    ..Default::default()
                },
            ),
            vec![0x1b, b'x'],
        );
    }

    #[test]
    fn test_key_to_bytes_f1() {
        assert_eq!(key_to_bytes(Key::F(1), Modifiers::default()), b"\x1bOP");
    }

    #[test]
    fn test_terminal_new_and_resize() {
        let mut term = SshTerminal::new(80, 24);
        assert_eq!(term.size, (80, 24));
        term.resize(120, 40);
        assert_eq!(term.size, (120, 40));
    }

    #[test]
    fn test_terminal_process_bytes() {
        let mut term = SshTerminal::new(80, 24);
        term.process_bytes(b"hello world");
        let screen = term.screen();
        let contents = screen.contents();
        assert!(contents.contains("hello world"));
    }

    #[test]
    fn test_agent_cli_command_default_perms() {
        use sandbox::Permissions;
        assert_eq!(
            agent_cli_command("claude", Permissions::Default, false, false, None),
            Some("claude".to_string()),
        );
        assert_eq!(
            agent_cli_command("goose", Permissions::Default, false, false, None),
            Some("goose session".to_string()),
        );
        assert_eq!(
            agent_cli_command("codex", Permissions::Default, false, false, None),
            Some("codex".to_string()),
        );
        assert_eq!(
            agent_cli_command("cursor", Permissions::Default, false, false, None),
            Some("cursor-agent".to_string()),
        );
        assert_eq!(agent_cli_command("unknown", Permissions::Default, false, false, None), None);
    }

    #[test]
    fn test_agent_cli_command_allow_all_interactive() {
        use sandbox::Permissions;
        let cmd = agent_cli_command("claude", Permissions::AllowAll, false, false, None).unwrap();
        assert!(cmd.contains("--dangerously-skip-permissions"));
        assert!(cmd.contains("--mcp-config"));
        assert!(cmd.contains("--add-dir"));
        assert!(!cmd.contains(" -p ")); // not headless (space-delimited to avoid matching inside --dangerously-skip-permissions)

        let cmd = agent_cli_command("codex", Permissions::AllowAll, false, false, None).unwrap();
        assert!(cmd.contains("--yolo"));

        let cmd = agent_cli_command("cursor", Permissions::AllowAll, false, false, None).unwrap();
        assert!(cmd.contains("--force"));
        // --trust requires headless mode (--print)
        assert!(!cmd.contains("--trust"));
    }

    #[test]
    fn test_agent_cli_command_accept_edits() {
        use sandbox::Permissions;
        let cmd = agent_cli_command("claude", Permissions::AcceptEdits, false, false, None).unwrap();
        assert!(cmd.contains("--permission-mode"));
        assert!(cmd.contains("acceptEdits"));

        let cmd = agent_cli_command("codex", Permissions::AcceptEdits, false, false, None).unwrap();
        assert!(cmd.contains("--full-auto"));

        // --trust requires headless mode, so not present in interactive
        let cmd = agent_cli_command("cursor", Permissions::AcceptEdits, false, false, None).unwrap();
        assert!(!cmd.contains("--trust"));
        assert!(!cmd.contains("--force"));
    }

    #[test]
    fn test_agent_cli_command_headless() {
        use sandbox::Permissions;
        // Headless mode should use -p/exec + stream-json + AllowAll
        let cmd = agent_cli_command("claude", Permissions::Default, true, false, None).unwrap();
        assert!(cmd.contains("-p"));
        assert!(cmd.contains("stream-json"));
        assert!(cmd.contains("--dangerously-skip-permissions")); // auto_mode forces AllowAll
        assert!(cmd.contains("$NANOSB_PROMPT"));

        let cmd = agent_cli_command("codex", Permissions::Default, true, false, None).unwrap();
        assert!(cmd.contains("exec"));
        assert!(cmd.contains("--json"));
        assert!(cmd.contains("--yolo")); // auto_mode forces AllowAll

        let cmd = agent_cli_command("goose", Permissions::Default, true, false, None).unwrap();
        assert!(cmd.contains("goose run"));
        assert!(cmd.contains("stream-json"));

        let cmd = agent_cli_command("cursor", Permissions::Default, true, false, None).unwrap();
        assert!(cmd.contains("-p"));
        assert!(cmd.contains("stream-json"));
        assert!(cmd.contains("--force")); // auto_mode forces AllowAll
        assert!(cmd.contains("--trust")); // headless mode allows --trust
        assert!(cmd.contains("--approve-mcps"));
    }

    #[test]
    fn test_agent_cli_command_resumed_interactive() {
        use sandbox::Permissions;
        assert_eq!(
            agent_cli_command("claude", Permissions::Default, false, true, None),
            Some("claude -c".to_string()),
        );
        assert_eq!(
            agent_cli_command("goose", Permissions::Default, false, true, None),
            Some("goose session -r".to_string()),
        );
        let cmd = agent_cli_command("codex", Permissions::Default, false, true, None).unwrap();
        assert!(cmd.contains("resume --last"));
        assert_eq!(
            agent_cli_command("cursor", Permissions::Default, false, true, None),
            Some("cursor-agent --continue".to_string()),
        );
    }

    #[test]
    fn test_agent_cli_command_resumed_allow_all() {
        use sandbox::Permissions;
        let cmd = agent_cli_command("claude", Permissions::AllowAll, false, true, None).unwrap();
        assert!(cmd.contains("-c"));
        assert!(cmd.contains("--dangerously-skip-permissions"));

        let cmd = agent_cli_command("codex", Permissions::AllowAll, false, true, None).unwrap();
        assert!(cmd.contains("resume --last"));
        assert!(cmd.contains("--yolo"));

        let cmd = agent_cli_command("cursor", Permissions::AllowAll, false, true, None).unwrap();
        assert!(cmd.contains("--continue"));
        assert!(cmd.contains("--force"));
    }

    #[test]
    fn test_agent_cli_command_selected_session_id() {
        use sandbox::Permissions;

        let claude = agent_cli_command_with_session(
            "claude",
            Permissions::Default,
            false,
            true,
            Some("sess-claude-123"),
            None,
            true,
        )
        .unwrap();
        assert!(claude.contains("--resume sess-claude-123"));
        assert!(!claude.contains(" -c"));

        let codex = agent_cli_command_with_session(
            "codex",
            Permissions::Default,
            false,
            true,
            Some("sess-codex-456"),
            None,
            true,
        )
        .unwrap();
        assert!(codex.contains("resume sess-codex-456"));
        assert!(!codex.contains("resume --last"));
    }

    #[test]
    fn test_agent_cli_command_with_model_claude() {
        use sandbox::Permissions;
        let cmd = agent_cli_command("claude", Permissions::Default, false, false, Some("claude-sonnet-4-5-20250929")).unwrap();
        assert!(cmd.contains("--model claude-sonnet-4-5-20250929"));
    }

    #[test]
    fn test_agent_cli_command_with_model_codex() {
        use sandbox::Permissions;
        let cmd = agent_cli_command("codex", Permissions::Default, false, false, Some("o4-mini")).unwrap();
        assert!(cmd.contains("--model o4-mini"));
    }

    #[test]
    fn test_agent_cli_command_with_model_cursor() {
        use sandbox::Permissions;
        let cmd = agent_cli_command("cursor", Permissions::Default, false, false, Some("sonnet-4.6")).unwrap();
        assert!(cmd.contains("--model sonnet-4.6"));
    }

    #[test]
    fn test_agent_cli_command_goose_no_model_flag() {
        // Goose uses env var, not CLI flag — model should NOT appear in the command string.
        use sandbox::Permissions;
        let cmd = agent_cli_command("goose", Permissions::Default, false, false, Some("claude-sonnet-4-5-20250929")).unwrap();
        assert!(!cmd.contains("--model"));
    }

    #[test]
    fn test_agent_cli_command_goose_configure_when_no_provider() {
        use sandbox::Permissions;
        let cmd = agent_cli_command_with_session(
            "goose", Permissions::Default, false, false, None, None, false,
        ).unwrap();
        assert_eq!(cmd, "goose configure");
    }

    #[test]
    fn test_agent_cli_command_goose_session_when_has_provider() {
        use sandbox::Permissions;
        let cmd = agent_cli_command_with_session(
            "goose", Permissions::Default, false, false, None, None, true,
        ).unwrap();
        assert_eq!(cmd, "goose session");
    }

    #[test]
    fn test_agent_cli_command_goose_resume_ignores_provider() {
        // Resume always resumes, regardless of provider status
        use sandbox::Permissions;
        let cmd = agent_cli_command_with_session(
            "goose", Permissions::Default, false, true, Some("sess-123"), None, false,
        ).unwrap();
        assert!(cmd.contains("goose session --resume --session-id sess-123"));
    }

    #[test]
    fn test_agent_cli_command_model_in_headless() {
        use sandbox::Permissions;
        let cmd = agent_cli_command("claude", Permissions::Default, true, false, Some("claude-opus-4-20250514")).unwrap();
        assert!(cmd.contains("--model claude-opus-4-20250514"));
        assert!(cmd.contains("-p"));
    }

    #[test]
    fn test_extract_urls_simple() {
        let (urls, _) = extract_urls(b"Visit https://claude.ai/oauth/authorize?code=true to sign in", &[]);
        assert_eq!(urls, vec!["https://claude.ai/oauth/authorize?code=true"]);
    }

    #[test]
    fn test_extract_urls_no_urls() {
        let (urls, _) = extract_urls(b"No URLs here, just text", &[]);
        assert!(urls.is_empty());
    }

    #[test]
    fn test_extract_urls_multiple() {
        let data = b"First: https://example.com/a then https://example.com/b end";
        let (urls, _) = extract_urls(data, &[]);
        assert_eq!(urls, vec!["https://example.com/a", "https://example.com/b"]);
    }

    #[test]
    fn test_extract_urls_strips_trailing_punctuation() {
        let (urls, _) = extract_urls(b"See https://example.com/path. More text", &[]);
        assert_eq!(urls, vec!["https://example.com/path"]);
    }

    #[test]
    fn test_extract_urls_with_ansi_escape() {
        // URL followed by an ANSI escape sequence
        let data = b"https://claude.ai/oauth\x1b[0m rest";
        let (urls, _) = extract_urls(data, &[]);
        assert_eq!(urls, vec!["https://claude.ai/oauth"]);
    }

    #[test]
    fn test_extract_urls_cross_chunk() {
        let chunk1 = b"Click here: https://claude.ai/oa";
        let (urls1, buffer) = extract_urls(chunk1, &[]);
        // First chunk may find a partial URL
        // Second chunk completes it
        let chunk2 = b"uth/authorize?code=true to sign in";
        let (urls2, _) = extract_urls(chunk2, &buffer);
        // The full URL should be found in one of the two results
        let all_urls: Vec<String> = urls1.into_iter().chain(urls2).collect();
        assert!(all_urls.iter().any(|u| u.contains("https://claude.ai/oauth/authorize?code=true")));
    }

    #[test]
    fn test_extract_urls_long_oauth_url() {
        let url = "https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&scope=org%3Acreate_api_key";
        let data = format!("Browser didn't open? Use the url below to sign in\n\n{}\n\nPaste code here", url);
        let (urls, _) = extract_urls(data.as_bytes(), &[]);
        assert_eq!(urls.len(), 1);
        assert!(urls[0].starts_with("https://claude.ai/oauth/authorize"));
    }

    #[test]
    fn test_extract_urls_pty_line_wrapped() {
        // PTY wraps long URLs by inserting \r\n at column boundaries.
        // The URL scanner must rejoin these fragments into the complete URL.
        let data = b"Browser didn't open? Use the url below to sign in (c to copy)\r\n\r\nhttps://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response\r\n_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&scope=org%3Acre\r\nate_api_key+user%3Aprofile&code_challenge=abc123&state=xyz789\r\n\r\nPaste code here if prompted >";
        let (urls, _) = extract_urls(data, &[]);
        assert_eq!(urls.len(), 1, "should find exactly one URL, got: {:?}", urls);
        assert!(urls[0].contains("redirect_uri="), "URL should contain redirect_uri, got: {}", urls[0]);
        assert!(urls[0].contains("state=xyz789"), "URL should contain state param, got: {}", urls[0]);
    }

    #[test]
    fn test_extract_urls_nested_redirect_uri() {
        // The OAuth URL contains redirect_uri with an unencoded nested https:// URL.
        // Only the outer URL should be extracted, not the nested redirect_uri value.
        let data = b"https://claude.ai/oauth/authorize?code=true&redirect_uri=https://platform.claude.com/oauth/code/callback&scope=org\r\n\r\n";
        let (urls, _) = extract_urls(data, &[]);
        assert_eq!(urls.len(), 1, "should find exactly one URL (the outer OAuth URL), got: {:?}", urls);
        assert!(urls[0].starts_with("https://claude.ai/oauth/authorize"), "should be the OAuth URL, got: {}", urls[0]);
    }

    #[test]
    fn test_extract_urls_multi_chunk_no_duplicate_nested() {
        // Simulate the real scenario: long OAuth URL arrives in multiple SSH chunks.
        // The URL contains redirect_uri=https://platform.claude.com/...
        // The split happens right before "https://platform.claude.com" starts a new chunk.
        // The scanner should NOT open "https://platform.claude.com/..." as a separate URL.
        let chunk1 = b"https://claude.ai/oauth/authorize?code=true&client_id=abc&response_type=code&redirect_uri=";
        let (urls1, buf1) = extract_urls(chunk1, &[]);

        let chunk2 = b"https://platform.claude.com/oauth/code/callback&scope=org&state=xyz\r\n\r\nPaste code >";
        let (urls2, _) = extract_urls(chunk2, &buf1);

        let all_urls: Vec<String> = urls1.into_iter().chain(urls2).collect();
        // We should NOT have https://platform.claude.com as a standalone URL
        assert!(
            !all_urls.iter().any(|u| u.starts_with("https://platform.claude.com")),
            "Should not open nested redirect_uri as separate URL, got: {:?}",
            all_urls
        );
    }

    #[test]
    fn test_extract_urls_deferred_at_line_wrap_boundary() {
        // When data ends with \r\n (potential PTY line wrap), the URL should
        // NOT be emitted yet. It is carried in the buffer and completed when
        // the next chunk arrives with the continuation.
        let chunk1 = b"https://claude.ai/oauth/authorize?code=true&response\r\n";
        let (urls1, buffer) = extract_urls(chunk1, &[]);
        assert!(urls1.is_empty(), "Should defer URL at \\r\\n boundary, got: {:?}", urls1);

        let chunk2 = b"_type=code&state=xyz789\r\n\r\nPaste code >";
        let (urls2, _) = extract_urls(chunk2, &buffer);
        assert_eq!(urls2.len(), 1, "Should find complete URL in second chunk, got: {:?}", urls2);
        assert!(urls2[0].contains("response_type=code"), "Should rejoin across chunks: {}", urls2[0]);
        assert!(urls2[0].contains("state=xyz789"), "Should have full URL: {}", urls2[0]);
    }

    #[test]
    fn test_extract_urls_deferred_at_end_of_data() {
        // When data ends mid-URL (no terminator), defer emission.
        let chunk1 = b"https://example.com/pa";
        let (urls1, buffer) = extract_urls(chunk1, &[]);
        assert!(urls1.is_empty(), "Should defer URL at end of data");

        let chunk2 = b"th?q=1 rest of text";
        let (urls2, _) = extract_urls(chunk2, &buffer);
        assert_eq!(urls2.len(), 1);
        assert_eq!(urls2[0], "https://example.com/path?q=1");
    }

    #[test]
    fn test_extract_urls_large_chunk_nested_redirect() {
        // Real scenario: the OAuth URL is >256 bytes and spans two SSH chunks.
        // The chunk boundary falls right at "redirect_uri=" so chunk2 starts with
        // "https://platform.claude.com/..." which looks like a standalone URL.
        // The 256-byte buffer from chunk1 only captures the URL tail, not the beginning.
        //
        // The fix: when https:// appears immediately after '=' (query param value),
        // skip it as a nested URL.

        // Build chunk1: lots of preamble + start of long OAuth URL ending at redirect_uri=
        let mut chunk1 = Vec::new();
        chunk1.extend_from_slice(b"Welcome to sandbox\r\nroot@localhost:~# claude\r\n");
        chunk1.extend_from_slice(b"\x1b[?2004h\x1b[38;5;141m"); // ANSI codes
        chunk1.extend_from_slice(&[b'A'; 400]); // more ANSI/banner output
        chunk1.extend_from_slice(b"\r\nBrowser didn't open? Use the url below to sign in\r\n\r\n");
        chunk1.extend_from_slice(b"https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=");
        // chunk1 is >700 bytes. 256-byte buffer captures only the tail.
        let (_urls1, buf1) = extract_urls(&chunk1, &[]);

        // chunk2 starts with the redirect_uri VALUE (a nested URL)
        let chunk2 = b"https://platform.claude.com/oauth/code/callback&scope=org%3Acreate_api_key+user%3Aprofile&code_challenge=abc&state=xyz\r\n\r\nPaste code here if prompted >";
        let (urls2, _) = extract_urls(chunk2, &buf1);

        // urls2 should NOT contain https://platform.claude.com as a standalone URL
        assert!(
            !urls2.iter().any(|u| u.starts_with("https://platform.claude.com")),
            "Should not open nested redirect_uri as separate URL, got: {:?}",
            urls2
        );
    }

    #[test]
    fn test_extract_urls_from_screen_tui_rendered() {
        // Simulate a TUI app (e.g. Claude Code / ink) rendering a long OAuth URL
        // using cursor positioning. The raw SSH data contains ANSI escapes between
        // URL line fragments which previously caused truncation.
        let mut parser = vt100::Parser::new(24, 80, 0);

        // Write the URL across multiple screen rows using cursor positioning
        // (simulating how ink/ratatui renders wrapped text).
        parser.process(b"\x1b[5;1HBrowser didn't open? Use the url below to sign in");
        parser.process(b"\x1b[7;1Hhttps://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-");
        parser.process(b"\x1b[8;1H5944d1962f5e&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com");
        parser.process(b"\x1b[9;1H%2Foauth%2Fcode%2Fcallback&scope=org%3Acreate_api_key&state=JeexkYr0rbPV2DlpQ2");
        parser.process(b"\x1b[11;1HPaste code here if prompted >");

        let urls = extract_urls_from_screen(parser.screen());

        assert_eq!(urls.len(), 1, "should find exactly one URL, got: {:?}", urls);
        assert!(
            urls[0].contains("client_id=9d1c250a"),
            "URL should contain client_id, got: {}",
            urls[0]
        );
        assert!(
            urls[0].contains("state=JeexkYr0rbPV2DlpQ2"),
            "URL should contain full state param, got: {}",
            urls[0]
        );
    }

    #[test]
    fn test_extract_urls_from_screen_with_borders() {
        // Simulate a TUI app that draws box borders around the URL area.
        let mut parser = vt100::Parser::new(24, 60, 0);

        parser.process("┌──────────────────────────────────────────────────────────┐".as_bytes());
        parser.process(b"\x1b[2;1H");
        parser.process("│ https://claude.ai/oauth/authorize?code=true&client_i │".as_bytes());
        parser.process(b"\x1b[3;1H");
        parser.process("│ d=abc-123&response_type=code&state=xyz               │".as_bytes());
        parser.process(b"\x1b[4;1H");
        parser.process("│                                                      │".as_bytes());
        parser.process(b"\x1b[5;1H");
        parser.process("│ Paste code here if prompted >                        │".as_bytes());
        parser.process(b"\x1b[6;1H");
        parser.process("└──────────────────────────────────────────────────────────┘".as_bytes());

        let urls = extract_urls_from_screen(parser.screen());

        assert_eq!(urls.len(), 1, "should find exactly one URL, got: {:?}", urls);
        assert!(
            urls[0].contains("state=xyz"),
            "URL should contain state param, got: {}",
            urls[0]
        );
    }

    #[test]
    fn test_url_dedup_key_auth_url_host_only() {
        // Auth URLs should dedup by host only, so truncated and full URLs match.
        let truncated = "https://auth.openai.com/ous";
        let full = "https://auth.openai.com/oauth/authorize?client_id=abc&scope=openid";
        assert_eq!(
            url_dedup_key(truncated),
            url_dedup_key(full),
            "Truncated and full auth URLs should have the same dedup key"
        );
        assert_eq!(url_dedup_key(full), "https://auth.openai.com");
    }

    #[test]
    fn test_url_dedup_key_non_auth_url_keeps_path() {
        // Non-auth URLs should keep host+path in the dedup key.
        let url1 = "https://example.com/page1?q=1";
        let url2 = "https://example.com/page2?q=2";
        assert_ne!(
            url_dedup_key(url1),
            url_dedup_key(url2),
            "Non-auth URLs with different paths should have different dedup keys"
        );
        assert_eq!(url_dedup_key(url1), "https://example.com/page1");
    }

    #[test]
    fn test_url_dedup_key_all_auth_domains() {
        // All whitelisted auth domains should use host-only dedup.
        for domain in AUTH_DOMAINS {
            let url = format!("https://{}/some/path?param=val", domain);
            assert_eq!(
                url_dedup_key(&url),
                format!("https://{}", domain),
                "Auth domain {} should use host-only dedup",
                domain
            );
        }
    }

    #[test]
    fn test_is_auth_url_cursor_com() {
        assert!(is_auth_url("https://cursor.com/loginlink?user_code=ABCD-1234"));
        assert!(is_auth_url("https://www.cursor.com/auth?client_id=123"));
        assert!(is_auth_url("https://authenticator.cursor.sh/authorize?code=abc"));
        assert!(is_auth_url("https://cursor.sh/device?code=xyz"));
        // Non-cursor domain should not match.
        assert!(!is_auth_url("https://evil-cursor.com/login"));
    }

    #[test]
    fn test_is_device_code_url() {
        assert!(is_device_code_url("https://github.com/login/device"));
        assert!(is_device_code_url("https://cursor.com/loginlink"));
        assert!(is_device_code_url("https://example.com/device"));
        assert!(is_device_code_url("https://example.com/login/device/"));
        // URLs with query params still match.
        assert!(is_device_code_url("https://cursor.com/loginlink?user_code=ABCD"));
        // Non-device paths should not match.
        assert!(!is_device_code_url("https://cursor.com/settings"));
        assert!(!is_device_code_url("https://cursor.com/"));
        // HTTP not supported.
        assert!(!is_device_code_url("http://cursor.com/loginlink"));
    }
}
