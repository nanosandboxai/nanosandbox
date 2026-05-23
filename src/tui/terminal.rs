//! Embedded SSH terminal adapter for the TUI.

use std::collections::HashMap;
use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

use super::event::AppEvent;

pub use terminal_core::{
    agent_cli_command, agent_cli_command_with_session, agent_env_vars, extract_oauth_callback_port,
    extract_urls, extract_urls_from_screen, is_auth_url, is_device_code_url, key_to_bytes,
    url_dedup_key, Key, Modifiers, SshTerminal, SshTerminalHandle,
};

/// Convert a crossterm `KeyEvent` into the byte sequence expected by a remote PTY.
pub fn crossterm_key_to_bytes(key: KeyEvent) -> Vec<u8> {
    key_to_bytes(map_key_code(key.code), map_modifiers(key.modifiers))
}

fn map_key_code(code: KeyCode) -> Key {
    match code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Tab => Key::Tab,
        KeyCode::Esc => Key::Esc,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Right => Key::Right,
        KeyCode::Left => Key::Left,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::F(n) => Key::F(n),
        _ => Key::Unknown,
    }
}

fn map_modifiers(mods: KeyModifiers) -> Modifiers {
    Modifiers {
        ctrl: mods.contains(KeyModifiers::CONTROL),
        alt: mods.contains(KeyModifiers::ALT),
        shift: mods.contains(KeyModifiers::SHIFT),
        meta: mods.contains(KeyModifiers::SUPER),
    }
}

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
    prompt: Option<&str>,
    is_resumed: bool,
    had_interaction: bool,
    selected_session_id: Option<&str>,
    model: Option<&str>,
    panel_idx: usize,
    tx: mpsc::UnboundedSender<AppEvent>,
) -> Result<SshTerminalHandle, anyhow::Error> {
    let (core_tx, mut core_rx) = mpsc::unbounded_channel::<terminal_core::TerminalEvent>();
    let app_tx = tx.clone();

    tokio::spawn(async move {
        while let Some(event) = core_rx.recv().await {
            match event {
                terminal_core::TerminalEvent::TerminalData { data } => {
                    let _ = app_tx.send(AppEvent::TerminalData { panel_idx, data });
                }
                terminal_core::TerminalEvent::SshDisconnected { error } => {
                    let _ = app_tx.send(AppEvent::SshDisconnected { panel_idx, error });
                }
            }
        }
    });

    terminal_core::connect_ssh(
        ssh_host,
        ssh_port,
        key_path,
        cols,
        rows,
        agent_name,
        env,
        workdir,
        permissions,
        auto_mode,
        prompt,
        is_resumed,
        had_interaction,
        selected_session_id,
        model,
        panel_idx,
        core_tx,
    )
    .await
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
            Err(e) => tracing::warn!(url = %url, error = %e, "Failed to spawn browser open command"),
        }
    }
    #[cfg(target_os = "windows")]
    {
        match std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => tracing::debug!(url = %url, "Spawned browser open command"),
            Err(e) => tracing::warn!(url = %url, error = %e, "Failed to spawn browser open command"),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        match std::process::Command::new("xdg-open")
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => tracing::debug!(url = %url, "Spawned browser open command"),
            Err(e) => tracing::warn!(url = %url, error = %e, "Failed to spawn browser open command"),
        }
    }
}
