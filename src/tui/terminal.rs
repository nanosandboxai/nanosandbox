//! Terminal rendering for TUI panels.
//!
//! Maintains terminal state via vt100 and provides handles for sending
//! keystrokes and resize events to a sandbox console.

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
            self.parser.screen_mut().set_scrollback(0);
        } else {
            // vt100 auto-increments scrollback_offset internally as new lines
            // push content into the scrollback buffer (grid.rs scroll logic),
            // but only caps to scrollback.len() — not to rows_len. If the
            // offset exceeds rows_len, visible_rows() underflows and panics:
            //   rows_len - scrollback_offset (usize subtraction overflow)
            // Re-sync our tracked value from vt100 and clamp to rows_len.
            let rows = self.size.1 as usize;
            self.scroll_offset = self.parser.screen().scrollback().min(rows);
            self.parser.screen_mut().set_scrollback(self.scroll_offset);
        }
    }

    /// Resize the terminal. Updates the vt100 parser dimensions.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        if (cols, rows) != self.size {
            // Reset scrollback before resize — content may not align after.
            if self.scroll_offset > 0 {
                self.scroll_offset = 0;
                self.parser.screen_mut().set_scrollback(0);
                self.auto_scroll = true;
            }
            self.size = (cols, rows);
            self.parser.screen_mut().set_size(rows, cols);
        }
    }

    /// Scroll up by N lines. Returns true if scroll position changed.
    pub fn scroll_up(&mut self, lines: usize) -> bool {
        let old = self.scroll_offset;
        // The vt100 crate's visible_rows() computes `rows.len() - scrollback_offset`,
        // so the offset must not exceed the terminal height. We also cap to the
        // actual scrollback buffer length.
        let rows = self.size.1 as usize;
        self.parser.screen_mut().set_scrollback(rows);
        let max = self.parser.screen().scrollback(); // clamped to min(rows, scrollback.len())
        self.scroll_offset = (old + lines).min(max);
        self.parser.screen_mut().set_scrollback(self.scroll_offset);
        self.auto_scroll = false;
        self.scroll_offset != old
    }

    /// Scroll down by N lines. Returns true if scroll position changed.
    pub fn scroll_down(&mut self, lines: usize) -> bool {
        let old = self.scroll_offset;
        self.scroll_offset = old.saturating_sub(lines);
        self.parser.screen_mut().set_scrollback(self.scroll_offset);
        if self.scroll_offset == 0 {
            self.auto_scroll = true;
        }
        self.scroll_offset != old
    }

    /// Jump to bottom (live screen).
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.parser.screen_mut().set_scrollback(0);
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
        self.parser.screen_mut().set_scrollback(rows);
        let max = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(saved);
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

/// Attach to a supervisor-managed sandbox console.
///
/// Console output is forwarded as `AppEvent::TerminalData`, and the returned
/// handle's channels are translated into attach-protocol input/resize frames.
pub async fn connect_console(
    sandbox_name: String,
    cols: u16,
    rows: u16,
    panel_idx: usize,
    tx: mpsc::UnboundedSender<AppEvent>,
) -> Result<SshTerminalHandle, String> {
    use crate::supervisor::client::{AttachConnection, SupervisorClient};
    use crate::supervisor::AttachFrame;
    use std::io::Write;

    let client = SupervisorClient::new(&sandbox_name);
    if !client.is_running() {
        return Err(format!(
            "supervised sandbox '{}' is not running",
            sandbox_name
        ));
    }

    let mut conn = AttachConnection::open(&client).map_err(|e| e.to_string())?;
    let mut writer = conn.writer_clone().map_err(|e| e.to_string())?;

    let mut initial = serde_json::json!({ "type": "resize", "cols": cols, "rows": rows })
        .to_string()
        .into_bytes();
    initial.push(b'\n');
    let _ = writer.write_all(&initial);

    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (resize_tx, mut resize_rx) = mpsc::unbounded_channel::<(u16, u16)>();

    tokio::spawn(async move {
        loop {
            let payload = tokio::select! {
                Some(bytes) = write_rx.recv() => {
                    let data = String::from_utf8_lossy(&bytes).to_string();
                    serde_json::json!({ "type": "input", "data": data }).to_string()
                }
                Some((cols, rows)) = resize_rx.recv() => {
                    serde_json::json!({ "type": "resize", "cols": cols, "rows": rows }).to_string()
                }
                else => break,
            };
            let mut frame = payload.into_bytes();
            frame.push(b'\n');
            if writer.write_all(&frame).is_err() {
                break;
            }
        }
    });

    let tx_read = tx.clone();
    tokio::task::spawn_blocking(move || loop {
        match conn.next_frame() {
            Ok(Some(AttachFrame::Output { data })) => {
                let _ = tx_read.send(AppEvent::TerminalData {
                    panel_idx,
                    data: data.into_bytes(),
                });
            }
            Ok(Some(AttachFrame::Exit { code })) => {
                let error = if code != 0 {
                    Some(format!("console exited with code {}", code))
                } else {
                    None
                };
                let _ = tx_read.send(AppEvent::SshDisconnected { panel_idx, error });
                break;
            }
            Ok(None) => {
                let _ = tx_read.send(AppEvent::SshDisconnected {
                    panel_idx,
                    error: None,
                });
                break;
            }
            Err(e) => {
                let _ = tx_read.send(AppEvent::SshDisconnected {
                    panel_idx,
                    error: Some(e.to_string()),
                });
                break;
            }
        }
    });

    Ok(SshTerminalHandle {
        write_tx,
        resize_tx,
    })
}

/// Attach to an agent running interactively through the in-guest exec agent's PTY.
///
/// Unlike [`connect_console`] (a non-TTY VM console), this starts the agent as a
/// child of the guest exec agent with a real PTY (`tty = true`), so interactive
/// TUIs (e.g. Claude Code) get a terminal. Output is forwarded as
/// `AppEvent::TerminalData`; the returned handle's channels drive stdin + resize.
///
/// `exec_sock` is the host socket bridged to the guest vsock exec port.
pub async fn connect_exec_pty(
    exec_sock: std::path::PathBuf,
    program: String,
    args: Vec<String>,
    cols: u16,
    rows: u16,
    panel_idx: usize,
    tx: mpsc::UnboundedSender<AppEvent>,
) -> Result<SshTerminalHandle, String> {
    use runtime::exec::{ExecClient, ExecEvent, ExecOptions};

    if !exec_sock.exists() {
        return Err(format!(
            "no exec socket at {} (interactive sandbox not ready)",
            exec_sock.display()
        ));
    }

    let client = ExecClient::new(exec_sock);
    let opts = ExecOptions::new().tty(true).size(cols.max(1), rows.max(1));
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let handle = client
        .start(&program, &arg_refs, opts)
        .map_err(|e| e.to_string())?;

    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (resize_tx, mut resize_rx) = mpsc::unbounded_channel::<(u16, u16)>();

    // A single blocking task owns the ExecHandle (it is not Send-shareable) and
    // multiplexes: drain pending input/resize, then read one guest event with a
    // short timeout so input stays responsive.
    let tx_read = tx.clone();
    tokio::task::spawn_blocking(move || {
        let mut handle = handle;
        loop {
            while let Ok(bytes) = write_rx.try_recv() {
                let s = String::from_utf8_lossy(&bytes).to_string();
                if handle.write_stdin(&s).is_err() {
                    return;
                }
            }
            while let Ok((c, r)) = resize_rx.try_recv() {
                let _ = handle.resize(c, r);
            }

            match handle.next_event(Some(std::time::Duration::from_millis(50))) {
                Ok(Some(ExecEvent::Output(chunk))) => {
                    let _ = tx_read.send(AppEvent::TerminalData {
                        panel_idx,
                        data: chunk.data.into_bytes(),
                    });
                }
                Ok(Some(ExecEvent::Exit { code })) => {
                    let error = if code != 0 {
                        Some(format!("agent exited with code {}", code))
                    } else {
                        None
                    };
                    let _ = tx_read.send(AppEvent::SshDisconnected { panel_idx, error });
                    return;
                }
                Ok(Some(ExecEvent::Error { message })) => {
                    let _ = tx_read.send(AppEvent::SshDisconnected {
                        panel_idx,
                        error: Some(message),
                    });
                    return;
                }
                Ok(Some(ExecEvent::Started { .. })) => {}
                Ok(None) => {
                    let _ = tx_read.send(AppEvent::SshDisconnected {
                        panel_idx,
                        error: None,
                    });
                    return;
                }
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        }
    });

    Ok(SshTerminalHandle {
        write_tx,
        resize_tx,
    })
}

/// Convert a crossterm `KeyEvent` into the byte sequence expected by a remote PTY.
pub fn crossterm_key_to_bytes(key: KeyEvent) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        KeyCode::Char(c) if ctrl => {
            // Ctrl+A..Z = 0x01..0x1A
            let byte = (c.to_ascii_lowercase() as u8)
                .wrapping_sub(b'a')
                .wrapping_add(1);
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

            if c == '\x1b'
                || c == '\x07'
                || c == '"'
                || c == '\''
                || c == '<'
                || c == '>'
                || c == ' '
                || c == '\t'
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
            let byte_offset: usize = chars[..url_char_start].iter().map(|c| c.len_utf8()).sum();
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
            line.trim_matches(|c: char| c.is_whitespace() || ('\u{2500}'..='\u{257F}').contains(&c))
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
    "claude.com",            // Claude Code OAuth authorize (new host)
    "www.claude.com",        // Claude web redirect/auth host (new host)
    "claude.ai",             // Claude Code CLI (OAuth authorize)
    "www.claude.ai",         // Claude web redirect/auth host
    "platform.claude.com",   // Claude OAuth callback/redirect host
    "auth.anthropic.com",    // Claude Code (token refresh)
    "console.anthropic.com", // Claude Code (console auth)
    // OpenAI Codex CLI
    "auth.openai.com",           // Codex CLI (OAuth authorize)
    "login.live.com",            // Codex CLI (Microsoft SSO)
    "login.microsoftonline.com", // Codex CLI (Azure AD SSO)
    // Cursor
    "authenticator.cursor.sh", // Cursor IDE (authenticator)
    "cursor.sh",               // Cursor IDE (legacy domain)
    "cursor.com",              // Cursor IDE (device flow sign-in)
    "www.cursor.com",          // Cursor IDE (www redirect)
    // GitHub (SSO provider for multiple agents)
    "github.com", // GitHub device flow (Codex, Copilot)
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
    "/login/device", // GitHub device flow
    "/loginlink",    // Cursor device flow
    "/device",       // Generic device code
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
            '-' | '.'
                | '_'
                | '~'
                | ':'
                | '/'
                | '?'
                | '#'
                | '['
                | ']'
                | '@'
                | '!'
                | '$'
                | '&'
                | '\''
                | '('
                | ')'
                | '*'
                | '+'
                | ','
                | ';'
                | '='
                | '%'
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

/// Open a URL in the host machine's default browser.
pub fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        match std::process::Command::new("open")
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => tracing::debug!(url = %url, "Spawned browser open command"),
            Err(e) => {
                tracing::warn!(url = %url, error = %e, "Failed to spawn browser open command")
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        match std::process::Command::new("xdg-open")
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => tracing::debug!(url = %url, "Spawned browser open command"),
            Err(e) => {
                tracing::warn!(url = %url, error = %e, "Failed to spawn browser open command")
            }
        }
    }
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
    fn test_extract_urls_simple() {
        let (urls, _) = extract_urls(
            b"Visit https://claude.ai/oauth/authorize?code=true to sign in",
            &[],
        );
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
        assert!(all_urls
            .iter()
            .any(|u| u.contains("https://claude.ai/oauth/authorize?code=true")));
    }

    #[test]
    fn test_extract_urls_long_oauth_url() {
        let url = "https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&scope=org%3Acreate_api_key";
        let data = format!(
            "Browser didn't open? Use the url below to sign in\n\n{}\n\nPaste code here",
            url
        );
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
        assert_eq!(
            urls.len(),
            1,
            "should find exactly one URL, got: {:?}",
            urls
        );
        assert!(
            urls[0].contains("redirect_uri="),
            "URL should contain redirect_uri, got: {}",
            urls[0]
        );
        assert!(
            urls[0].contains("state=xyz789"),
            "URL should contain state param, got: {}",
            urls[0]
        );
    }

    #[test]
    fn test_extract_urls_nested_redirect_uri() {
        // The OAuth URL contains redirect_uri with an unencoded nested https:// URL.
        // Only the outer URL should be extracted, not the nested redirect_uri value.
        let data = b"https://claude.ai/oauth/authorize?code=true&redirect_uri=https://platform.claude.com/oauth/code/callback&scope=org\r\n\r\n";
        let (urls, _) = extract_urls(data, &[]);
        assert_eq!(
            urls.len(),
            1,
            "should find exactly one URL (the outer OAuth URL), got: {:?}",
            urls
        );
        assert!(
            urls[0].starts_with("https://claude.ai/oauth/authorize"),
            "should be the OAuth URL, got: {}",
            urls[0]
        );
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
            !all_urls
                .iter()
                .any(|u| u.starts_with("https://platform.claude.com")),
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
        assert!(
            urls1.is_empty(),
            "Should defer URL at \\r\\n boundary, got: {:?}",
            urls1
        );

        let chunk2 = b"_type=code&state=xyz789\r\n\r\nPaste code >";
        let (urls2, _) = extract_urls(chunk2, &buffer);
        assert_eq!(
            urls2.len(),
            1,
            "Should find complete URL in second chunk, got: {:?}",
            urls2
        );
        assert!(
            urls2[0].contains("response_type=code"),
            "Should rejoin across chunks: {}",
            urls2[0]
        );
        assert!(
            urls2[0].contains("state=xyz789"),
            "Should have full URL: {}",
            urls2[0]
        );
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
            !urls2
                .iter()
                .any(|u| u.starts_with("https://platform.claude.com")),
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

        assert_eq!(
            urls.len(),
            1,
            "should find exactly one URL, got: {:?}",
            urls
        );
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

        assert_eq!(
            urls.len(),
            1,
            "should find exactly one URL, got: {:?}",
            urls
        );
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
        assert!(is_auth_url(
            "https://cursor.com/loginlink?user_code=ABCD-1234"
        ));
        assert!(is_auth_url("https://www.cursor.com/auth?client_id=123"));
        assert!(is_auth_url(
            "https://authenticator.cursor.sh/authorize?code=abc"
        ));
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
        assert!(is_device_code_url(
            "https://cursor.com/loginlink?user_code=ABCD"
        ));
        // Non-device paths should not match.
        assert!(!is_device_code_url("https://cursor.com/settings"));
        assert!(!is_device_code_url("https://cursor.com/"));
        // HTTP not supported.
        assert!(!is_device_code_url("http://cursor.com/loginlink"));
    }
}
