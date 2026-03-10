//! Embedded SSH terminal for TUI panels.
//!
//! Manages SSH connections to sandbox VMs, maintains terminal state via vt100,
//! and provides handles for sending keystrokes and resize events.

use std::collections::HashMap;
use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

use super::event::AppEvent;

/// Terminal state backed by a vt100 parser.
///
/// Holds the virtual terminal screen buffer that is rendered by
/// `tui_term::widget::PseudoTerminal` in the renderer.
pub struct SshTerminal {
    /// vt100 parser maintaining the terminal screen state.
    parser: vt100::Parser,
    /// Current terminal dimensions (cols, rows).
    size: (u16, u16),
}

impl SshTerminal {
    /// Create a new terminal with the given dimensions.
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, 1000),
            size: (cols, rows),
        }
    }

    /// Get the current screen state for rendering.
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Feed raw bytes from the SSH channel into the terminal parser.
    pub fn process_bytes(&mut self, data: &[u8]) {
        self.parser.process(data);
    }

    /// Resize the terminal. Updates the vt100 parser dimensions.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        if (cols, rows) != self.size {
            self.size = (cols, rows);
            self.parser.set_size(rows, cols);
        }
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
    ssh_port: u16,
    key_path: PathBuf,
    cols: u16,
    rows: u16,
    agent_name: &str,
    env: &HashMap<String, String>,
    workdir: Option<&str>,
    panel_idx: usize,
    tx: mpsc::UnboundedSender<AppEvent>,
) -> Result<SshTerminalHandle, anyhow::Error> {
    // Load the private key
    let key_data = tokio::fs::read_to_string(&key_path).await?;
    let key_pair = russh::keys::decode_secret_key(&key_data, None)?;
    let key_with_hash = russh::keys::PrivateKeyWithHashAlg::new(
        std::sync::Arc::new(key_pair),
        None, // hash_alg is ignored for ed25519
    );

    // Connect
    let config = russh::client::Config::default();
    let config = std::sync::Arc::new(config);
    let sh = SshHandler;
    let mut session =
        russh::client::connect(config, format!("127.0.0.1:{}", ssh_port), sh).await?;

    // Authenticate
    let auth_result = session
        .authenticate_publickey("root", key_with_hash)
        .await?;
    if !matches!(auth_result, russh::client::AuthResult::Success) {
        anyhow::bail!("SSH public key authentication failed");
    }

    // Open channel and request PTY + shell
    let channel = session.channel_open_session().await?;
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

    // Build the initialization commands (cd + env vars + agent CLI launch)
    let mut init_commands = String::new();
    if let Some(dir) = workdir {
        init_commands.push_str(&format!("cd '{}'\n", dir));
    }
    for (key, val) in env {
        init_commands.push_str(&format!("export {}='{}'\n", key, val.replace('\'', "'\\''")));
    }
    if let Some(cmd) = agent_cli_command(agent_name) {
        init_commands.push_str(&format!("{}\n", cmd));
    }

    // Split the channel into read and write halves
    let (mut read_half, write_half) = channel.split();

    // Create channels for keystroke and resize forwarding
    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (resize_tx, mut resize_rx) = mpsc::unbounded_channel::<(u16, u16)>();

    // Send init commands after a short delay (let shell start)
    {
        use tokio::io::AsyncWriteExt;
        let mut writer = write_half.make_writer();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let _ = writer.write_all(init_commands.as_bytes()).await;

            // Then enter the write loop: forward keystrokes + handle resize
            loop {
                tokio::select! {
                    Some(data) = write_rx.recv() => {
                        if writer.write_all(&data).await.is_err() {
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
    }

    // Spawn read loop: forwards SSH channel data to TUI events
    let tx_read = tx;
    tokio::spawn(async move {
        use russh::ChannelMsg;
        while let Some(msg) = read_half.wait().await {
            match msg {
                ChannelMsg::Data { data } => {
                    let _ = tx_read.send(AppEvent::TerminalData {
                        panel_idx,
                        data: data.to_vec(),
                    });
                }
                ChannelMsg::ExtendedData { data, .. } => {
                    let _ = tx_read.send(AppEvent::TerminalData {
                        panel_idx,
                        data: data.to_vec(),
                    });
                }
                ChannelMsg::Eof | ChannelMsg::Close => {
                    let _ = tx_read.send(AppEvent::SshDisconnected {
                        panel_idx,
                        error: None,
                    });
                    break;
                }
                ChannelMsg::ExitStatus { exit_status } => {
                    let _ = tx_read.send(AppEvent::SshDisconnected {
                        panel_idx,
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
    });

    Ok(SshTerminalHandle {
        write_tx,
        resize_tx,
    })
}

/// Map agent name to the CLI command to auto-launch after SSH connection.
fn agent_cli_command(agent_name: &str) -> Option<String> {
    match agent_name {
        "claude" | "claude-code" => Some("claude".to_string()),
        "opencode" => Some("opencode".to_string()),
        "goose" => Some("goose session".to_string()),
        "codex" => Some("codex".to_string()),
        "cursor" | "cursor-agent" => Some("cursor-agent".to_string()),
        _ => None,
    }
}

/// Convert a crossterm `KeyEvent` into the byte sequence expected by a remote PTY.
pub fn crossterm_key_to_bytes(key: KeyEvent) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        KeyCode::Char(c) if ctrl => {
            // Ctrl+A..Z = 0x01..0x1A
            let byte = (c.to_ascii_lowercase() as u8).wrapping_sub(b'a').wrapping_add(1);
            vec![byte]
        }
        KeyCode::Char(c) if alt => {
            // Alt+<key> = ESC followed by the key
            let mut buf = vec![0x1b];
            let mut char_buf = [0u8; 4];
            let s = c.encode_utf8(&mut char_buf);
            buf.extend_from_slice(s.as_bytes());
            buf
        }
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            s.as_bytes().to_vec()
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::F(1) => b"\x1bOP".to_vec(),
        KeyCode::F(2) => b"\x1bOQ".to_vec(),
        KeyCode::F(3) => b"\x1bOR".to_vec(),
        KeyCode::F(4) => b"\x1bOS".to_vec(),
        KeyCode::F(5) => b"\x1b[15~".to_vec(),
        KeyCode::F(6) => b"\x1b[17~".to_vec(),
        KeyCode::F(7) => b"\x1b[18~".to_vec(),
        KeyCode::F(8) => b"\x1b[19~".to_vec(),
        KeyCode::F(9) => b"\x1b[20~".to_vec(),
        KeyCode::F(10) => b"\x1b[21~".to_vec(),
        KeyCode::F(11) => b"\x1b[23~".to_vec(),
        KeyCode::F(12) => b"\x1b[24~".to_vec(),
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

/// Open a URL in the host machine's default browser.
pub fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(not(target_os = "macos"))]
    let cmd = "xdg-open";

    let _ = std::process::Command::new(cmd)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_to_bytes_char() {
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(crossterm_key_to_bytes(key), b"a");
    }

    #[test]
    fn test_key_to_bytes_enter() {
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(crossterm_key_to_bytes(key), b"\r");
    }

    #[test]
    fn test_key_to_bytes_ctrl_c() {
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(crossterm_key_to_bytes(key), vec![0x03]);
    }

    #[test]
    fn test_key_to_bytes_arrow_up() {
        let key = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(crossterm_key_to_bytes(key), b"\x1b[A");
    }

    #[test]
    fn test_key_to_bytes_backspace() {
        let key = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(crossterm_key_to_bytes(key), vec![0x7f]);
    }

    #[test]
    fn test_key_to_bytes_alt_char() {
        let key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT);
        assert_eq!(crossterm_key_to_bytes(key), vec![0x1b, b'x']);
    }

    #[test]
    fn test_key_to_bytes_f1() {
        let key = KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE);
        assert_eq!(crossterm_key_to_bytes(key), b"\x1bOP");
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
    fn test_agent_cli_command() {
        assert_eq!(agent_cli_command("claude"), Some("claude".to_string()));
        assert_eq!(agent_cli_command("goose"), Some("goose session".to_string()));
        assert_eq!(agent_cli_command("unknown"), None);
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
}
