//! Main event loop for the TUI.

use std::collections::HashMap;
use std::io::{self, IsTerminal};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::time::Duration;

use ratatui::crossterm::event::{
    Event as CrosstermEvent, KeyCode, KeyEvent, KeyModifiers,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::{mpsc, Mutex};

use crate::config::{McpServerConfig, SandboxConfig};
use crate::Sandbox;

use super::app::{AgentPanel, App, ChatMessage, InputFocus, MessageRole, PanelMode, SidebarFilesTab, SubmitResult};
use super::commands::{self, Command};
use super::event::{spawn_terminal_event_reader, AppEvent};
use super::renderer;

/// Run the TUI application.
///
/// This enables raw mode, enters the alternate screen, and runs the main
/// event loop. On exit (or error) it restores the terminal.
pub async fn run_tui(
    project_path: Option<std::path::PathBuf>,
    sandbox_configs: Vec<(String, crate::config::SandboxConfig)>,
) -> anyhow::Result<()> {
    // Check if we're running in a real terminal.
    if !io::stdout().is_terminal() {
        anyhow::bail!(
            "nanosb requires an interactive terminal to run.\n\
             Use 'nanosb <command>' for non-interactive usage (e.g., nanosb doctor, nanosb run)."
        );
    }

    // Install a silent logger early so that the libkrun FFI's
    // `env_logger::try_init_from_env()` call (inside Sandbox::create) finds
    // a logger already present and skips installing one that writes to stderr.
    // Without this, tracing INFO logs corrupt the ratatui alternate screen.
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("off"))
        .try_init();

    // Validate runtime prerequisites before launching TUI.
    println!("\nChecking runtime prerequisites...\n");
    let validation = crate::runtime::validate_runtime_prerequisites_detailed().await;

    print_validation_results(&validation);

    if !validation.is_ok() {
        println!("\nCannot start TUI. Fix the errors above.");
        #[cfg(target_os = "macos")]
        println!("Run './scripts/install/macos.sh' to install dependencies.");
        #[cfg(target_os = "linux")]
        println!("Run './scripts/install/linux.sh' to install dependencies.");
        println!("Run 'nanosb doctor' for full details.");
        anyhow::bail!("Runtime prerequisites not met.");
    }

    println!("\nReady. Starting TUI...\n");
    // Brief pause so the user can see the results
    tokio::time::sleep(Duration::from_millis(800)).await;

    // Redirect stderr to /dev/null before entering the alternate screen.
    // Native C libraries (libkrun, gvproxy) write to stderr via fprintf()
    // which bypasses Rust's logging. Without this redirect those writes
    // corrupt the ratatui alternate screen or cause panics when the
    // terminal buffer fills up (EAGAIN / os error 35).
    let saved_stderr = unsafe { libc::dup(io::stderr().as_raw_fd()) };
    let dev_null = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY) };
    if dev_null >= 0 {
        unsafe {
            libc::dup2(dev_null, libc::STDERR_FILENO);
            libc::close(dev_null);
        }
    }

    // Set up terminal.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Install a panic hook that restores the terminal before printing
    // the panic message. Without this, panics corrupt the alternate screen.
    let original_hook = std::panic::take_hook();
    let saved_stderr_for_hook = saved_stderr;
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        // Restore stderr so the panic message is visible.
        if saved_stderr_for_hook >= 0 {
            unsafe {
                libc::dup2(saved_stderr_for_hook, libc::STDERR_FILENO);
            }
        }
        original_hook(info);
    }));

    // Create app state.
    let mut app = App::new();
    app.project_path = project_path;

    // Create the event channel.
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

    // Spawn terminal event reader.
    spawn_terminal_event_reader(tx.clone());

    // Auto-start sandboxes from config file.
    for (key, config) in sandbox_configs {
        add_agent_from_config(&mut app, &key, config, &tx);
    }

    // Spawn tick timer (every 250ms).
    {
        let tick_tx = tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            loop {
                interval.tick().await;
                if tick_tx.send(AppEvent::Tick).is_err() {
                    break;
                }
            }
        });
    }

    // Initial render.
    terminal.draw(|frame| renderer::render(frame, &mut app))?;

    // Main event loop.
    while let Some(event) = rx.recv().await {
        match event {
            AppEvent::Terminal(crossterm_event) => {
                if let CrosstermEvent::Key(key) = crossterm_event {
                    handle_key_event(&mut app, key, &tx).await;
                }
            }
            AppEvent::AgentOutput(panel_idx, text, is_stderr) => {
                app.append_agent_output(panel_idx, &text, is_stderr);
            }
            AppEvent::AgentDone(panel_idx, exit_code) => {
                app.mark_agent_done(panel_idx, exit_code);
            }
            AppEvent::SandboxCreating { panel_idx, message } => {
                if let Some(panel) = app.panels.get_mut(panel_idx) {
                    panel.chat_history.push(ChatMessage {
                        role: MessageRole::System,
                        content: message,
                    });
                }
            }
            AppEvent::SandboxReady { panel_idx, sandbox, short_id, project_mount } => {
                // Get SSH info before storing sandbox
                let ssh_info = {
                    let sb = sandbox.lock().await;
                    let port = sb.ssh_port();
                    let key = sb.ssh_key_path();
                    port.zip(key)
                };

                if let Some(panel) = app.panels.get_mut(panel_idx) {
                    panel.sandbox = Some(sandbox);
                    panel.sandbox_id_short = short_id.clone();
                    panel.project_mount = project_mount;
                    panel.chat_history.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("Sandbox {} started. Connecting SSH terminal...", short_id),
                    });
                }
                // Initiate SSH connection if SSH info is available
                if let Some((ssh_port, key_path)) = ssh_info {
                    if let Some(panel) = app.panels.get(panel_idx) {
                        let agent_name = panel.agent_name.clone();
                        let env = panel.env.clone();
                        let workdir = panel.project_mount.as_ref().map(|_| "/workspace".to_string());
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            // Small delay for sshd to be fully ready
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            match super::terminal::connect_ssh(
                                ssh_port, key_path, 80, 24,
                                &agent_name, &env, workdir.as_deref(), panel_idx, tx.clone(),
                            ).await {
                                Ok(handle) => {
                                    let _ = tx.send(AppEvent::SshConnected { panel_idx, handle });
                                }
                                Err(e) => {
                                    let _ = tx.send(AppEvent::SshDisconnected {
                                        panel_idx,
                                        error: Some(format!("SSH connect failed: {}", e)),
                                    });
                                }
                            }
                        });
                    }
                }
            }
            AppEvent::SandboxFailed { panel_idx, error } => {
                if let Some(panel) = app.panels.get_mut(panel_idx) {
                    panel.chat_history.push(ChatMessage {
                        role: MessageRole::System,
                        content: error,
                    });
                }
            }
            AppEvent::SshConnected { panel_idx, handle } => {
                if let Some(panel) = app.panels.get_mut(panel_idx) {
                    let (cols, rows) = panel.last_terminal_size;
                    panel.terminal = Some(super::terminal::SshTerminal::new(cols, rows));
                    panel.terminal_handle = Some(handle);
                    panel.mode = PanelMode::Terminal;
                    // Clear chat history - terminal takes over the display
                    panel.chat_history.clear();
                }
            }
            AppEvent::TerminalData { panel_idx, data } => {
                if let Some(panel) = app.panels.get_mut(panel_idx) {
                    if let Some(ref mut term) = panel.terminal {
                        term.process_bytes(&data);

                        // Scan the parsed screen for URLs and auto-open in host browser.
                        // Using the vt100 screen contents (not raw bytes) so that ANSI
                        // escape sequences from TUI apps don't truncate long URLs.
                        let urls =
                            super::terminal::extract_urls_from_screen(term.screen());
                        for url in urls {
                            if panel.opened_urls.insert(url.clone()) {
                                super::terminal::open_url_in_browser(&url);
                            }
                        }
                    }
                }
            }
            AppEvent::SshDisconnected { panel_idx, error } => {
                if let Some(panel) = app.panels.get_mut(panel_idx) {
                    panel.mode = PanelMode::Agent;
                    panel.terminal = None;
                    panel.terminal_handle = None;
                    if let Some(err) = error {
                        panel.chat_history.push(ChatMessage {
                            role: MessageRole::System,
                            content: format!("SSH disconnected: {}", err),
                        });
                    } else {
                        panel.chat_history.push(ChatMessage {
                            role: MessageRole::System,
                            content: "SSH session ended.".to_string(),
                        });
                    }
                }
            }
            AppEvent::Tick => {
                // Tick down temporary status message.
                if let Some((_, ref mut ticks)) = app.status_message {
                    *ticks = ticks.saturating_sub(1);
                    if *ticks == 0 {
                        app.status_message = None;
                    }
                }
                app.sidebar_tick_counter = app.sidebar_tick_counter.wrapping_add(1);
                if app.sidebar_tick_counter.is_multiple_of(8) {
                    // Refresh file lists when sidebar is visible (~every 2s).
                    if app.show_sandbox_sidebar {
                        app.refresh_sidebar_modified_files();
                        app.refresh_sidebar_committed_files();
                    }
                    // Auto-sync commits from all panel clones to source repos.
                    let notifications = app.sync_project_commits();
                    for (panel_idx, message) in notifications {
                        if let Some(panel) = app.panels.get_mut(panel_idx) {
                            panel.chat_history.push(ChatMessage {
                                role: MessageRole::System,
                                content: message,
                            });
                        }
                    }
                }
            }
            AppEvent::OpenTuiTool { binary, path } => {
                // Suspend TUI: leave alternate screen, disable raw mode
                let _ = disable_raw_mode();
                let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);

                // Restore stderr so the tool can use it
                if saved_stderr >= 0 {
                    unsafe { libc::dup2(saved_stderr, libc::STDERR_FILENO); }
                }

                // Build tool-specific arguments
                let path_str = path.to_string_lossy().to_string();
                let mut cmd = std::process::Command::new(&binary);
                match binary.as_str() {
                    "gitui" => { cmd.args(["-d", &path_str]); }
                    "lazygit" => { cmd.args(["-p", &path_str]); }
                    "tig" => { cmd.current_dir(&path); }
                    _ => { cmd.arg(&path); }
                };
                // Block until tool exits
                let _ = cmd.status();

                // Redirect stderr back to /dev/null
                let dev_null = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY) };
                if dev_null >= 0 {
                    unsafe {
                        libc::dup2(dev_null, libc::STDERR_FILENO);
                        libc::close(dev_null);
                    }
                }

                // Resume TUI: enter alternate screen, enable raw mode
                let _ = enable_raw_mode();
                let _ = execute!(terminal.backend_mut(), EnterAlternateScreen);
                terminal.clear()?;
            }
        }

        if app.should_quit {
            break;
        }

        // Re-render after every event.
        terminal.draw(|frame| renderer::render(frame, &mut app))?;
    }

    // Restore terminal before cleanup so the user sees progress messages.
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    // Restore stderr so cleanup log messages are visible.
    if saved_stderr >= 0 {
        unsafe {
            libc::dup2(saved_stderr, libc::STDERR_FILENO);
            libc::close(saved_stderr);
        }
    }

    // Clean up all running sandboxes (kill VMs, stop gvproxy, remove SSH keys).
    // Teardown project mounts first (auto-commit and fetch clones).
    for panel in &mut app.panels {
        if let Some(mut pm) = panel.project_mount.take() {
            let _ = pm.teardown();
        }
    }

    let sandbox_count = app
        .panels
        .iter()
        .filter(|p| p.sandbox.is_some())
        .count();
    if sandbox_count > 0 {
        eprintln!("Shutting down {} sandbox(es)...", sandbox_count);

        let mut handles = Vec::new();
        for panel in &mut app.panels {
            if let Some(sb_arc) = panel.sandbox.take() {
                handles.push(tokio::spawn(async move {
                    match Arc::try_unwrap(sb_arc) {
                        Ok(mutex) => {
                            let sandbox = mutex.into_inner();
                            let _ = sandbox.destroy().await;
                        }
                        Err(arc) => {
                            let mut sb = arc.lock().await;
                            let _ = sb.stop().await;
                        }
                    }
                }));
            }
        }

        // Wait for all sandbox cleanups to complete (with timeout).
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(15);
        for handle in handles {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let _ = tokio::time::timeout(remaining, handle).await;
        }

        eprintln!("All sandboxes stopped.");
    }

    Ok(())
}

/// Print validation results as a checklist.
fn print_validation_results(validation: &crate::runtime::ValidationResult) {
    for err in &validation.errors {
        println!("  [x] {}: {}", err.check, err.message);
        if let Some(ref hint) = err.fix_hint {
            println!("      Fix: {}", hint);
        }
    }
    for warning in &validation.warnings {
        println!("  [!] {}", warning);
    }

    // Show passed checks
    #[cfg(target_os = "macos")]
    {
        let checks = ["Architecture", "libkrun Library", "Hypervisor.framework", "gvproxy"];
        for name in &checks {
            let failed = validation.errors.iter().any(|e| e.check == *name);
            let warned = validation.warnings.iter().any(|w| w.contains(name));
            if !failed && !warned {
                println!("  [v] {}", name);
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        let checks = ["libkrun Library", "KVM Device", "gvproxy"];
        for name in &checks {
            let failed = validation.errors.iter().any(|e| e.check == *name);
            let warned = validation.warnings.iter().any(|w| w.contains(name));
            if !failed && !warned {
                println!("  [v] {}", name);
            }
        }
    }
}

/// Handle a single key event.
async fn handle_key_event(
    app: &mut App,
    key: KeyEvent,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    // Terminal mode: forward keystrokes to SSH, intercept only navigation keys.
    if app.input_focus == InputFocus::Panel {
        if let Some(panel) = app.panels.get(app.focused_panel) {
            if panel.mode == PanelMode::Terminal && panel.terminal_handle.is_some() {
                match key.code {
                    // Intercept panel navigation keys
                    KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        app.focus_prev();
                        return;
                    }
                    KeyCode::BackTab => {
                        app.focus_prev();
                        return;
                    }
                    KeyCode::Tab => {
                        app.focus_next();
                        return;
                    }
                    KeyCode::Esc => {
                        app.focus_global();
                        return;
                    }
                    // Ctrl+F: toggle zoom
                    KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if !app.panels.is_empty() {
                            app.zoomed = !app.zoomed;
                        }
                        return;
                    }
                    // Sidebar navigation: Ctrl+Arrow keys
                    KeyCode::Left | KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if app.show_sandbox_sidebar {
                            app.sidebar_files_tab = match app.sidebar_files_tab {
                                SidebarFilesTab::Modified => SidebarFilesTab::Committed,
                                SidebarFilesTab::Committed => SidebarFilesTab::Modified,
                            };
                        }
                        return;
                    }
                    KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if app.show_sandbox_sidebar {
                            match app.sidebar_files_tab {
                                SidebarFilesTab::Modified => {
                                    app.sidebar_files_scroll = app.sidebar_files_scroll.saturating_sub(1);
                                }
                                SidebarFilesTab::Committed => {
                                    app.sidebar_committed_scroll = app.sidebar_committed_scroll.saturating_sub(1);
                                }
                            }
                        }
                        return;
                    }
                    KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if app.show_sandbox_sidebar {
                            match app.sidebar_files_tab {
                                SidebarFilesTab::Modified => {
                                    if app.sidebar_files_scroll + 1 < app.sidebar_modified_files.len() {
                                        app.sidebar_files_scroll += 1;
                                    }
                                }
                                SidebarFilesTab::Committed => {
                                    if app.sidebar_committed_scroll + 1 < app.sidebar_committed_files.len() {
                                        app.sidebar_committed_scroll += 1;
                                    }
                                }
                            }
                        }
                        return;
                    }
                    _ => {
                        // Forward everything else to SSH terminal
                        let bytes = super::terminal::crossterm_key_to_bytes(key);
                        if !bytes.is_empty() {
                            if let Some(panel) = app.panels.get(app.focused_panel) {
                                if let Some(ref handle) = panel.terminal_handle {
                                    let _ = handle.write_tx.send(bytes);
                                }
                            }
                        }
                        return;
                    }
                }
            }
        }
    }

    // Sidebar: Ctrl+Up/Down scrolls files, Ctrl+Left/Right switches tabs.
    if app.show_sandbox_sidebar && key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Up => {
                match app.sidebar_files_tab {
                    SidebarFilesTab::Modified => {
                        app.sidebar_files_scroll = app.sidebar_files_scroll.saturating_sub(1);
                    }
                    SidebarFilesTab::Committed => {
                        app.sidebar_committed_scroll = app.sidebar_committed_scroll.saturating_sub(1);
                    }
                }
                return;
            }
            KeyCode::Down => {
                match app.sidebar_files_tab {
                    SidebarFilesTab::Modified => {
                        if app.sidebar_files_scroll + 1 < app.sidebar_modified_files.len() {
                            app.sidebar_files_scroll += 1;
                        }
                    }
                    SidebarFilesTab::Committed => {
                        if app.sidebar_committed_scroll + 1 < app.sidebar_committed_files.len() {
                            app.sidebar_committed_scroll += 1;
                        }
                    }
                }
                return;
            }
            KeyCode::Left | KeyCode::Right => {
                app.sidebar_files_tab = match app.sidebar_files_tab {
                    SidebarFilesTab::Modified => SidebarFilesTab::Committed,
                    SidebarFilesTab::Committed => SidebarFilesTab::Modified,
                };
                return;
            }
            _ => {}
        }
    }

    match key.code {
        // Tab / Shift+Tab: cycle focus.
        KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.focus_prev();
        }
        KeyCode::BackTab => {
            app.focus_prev();
        }
        KeyCode::Tab => {
            app.focus_next();
        }

        // Esc: dismiss autocomplete → return to global bar → close sidebar.
        KeyCode::Esc => {
            if app.autocomplete_index.is_some() {
                app.autocomplete_index = None;
            } else if app.input_focus == InputFocus::Panel {
                app.focus_global();
            } else {
                app.show_mcp_sidebar = false;
            }
        }

        // Shift+Enter or Alt+Enter: insert newline.
        KeyCode::Enter
            if key.modifiers.contains(KeyModifiers::SHIFT)
                || key.modifiers.contains(KeyModifiers::ALT) =>
        {
            app.handle_newline();
        }

        // Enter: accept autocomplete selection or submit.
        KeyCode::Enter => {
            // If autocomplete has a selected item, fill it into input.
            if app.autocomplete_active() {
                if let Some(idx) = app.autocomplete_index {
                    let suggestions = commands::autocomplete(app.current_input());
                    if let Some(selected) = suggestions.get(idx) {
                        let cmd_text = format!("{} ", selected);
                        match app.input_focus {
                            InputFocus::Global => {
                                app.global_input.set_text(cmd_text);
                            }
                            InputFocus::Panel => {
                                if let Some(panel) = app.focused_panel_mut() {
                                    panel.input.set_text(cmd_text);
                                }
                            }
                        }
                        app.autocomplete_index = None;
                        return;
                    }
                }
            }

            // Reset scroll to bottom so the user sees command output / their message.
            if let Some(panel) = app.focused_panel_mut() {
                panel.scroll_offset = 0;
            }

            let result = app.handle_submit();
            match result {
                SubmitResult::Command(cmd) => {
                    handle_command(app, cmd, tx).await;
                }
                SubmitResult::CommandError(msg) => {
                    let err = ChatMessage {
                        role: MessageRole::System,
                        content: msg,
                    };
                    if let Some(panel) = app.focused_panel_mut() {
                        panel.chat_history.push(err);
                    } else {
                        app.system_messages.push(err);
                    }
                }
                SubmitResult::Message(msg) => {
                    handle_message(app, &msg, tx);
                }
                SubmitResult::Empty | SubmitResult::NoPanel => {
                    // Nothing to do.
                }
            }
        }

        // Cursor movement: left/right.
        KeyCode::Left => {
            app.handle_move_left();
        }
        KeyCode::Right => {
            app.handle_move_right();
        }

        // Home/End: move to start/end of current logical line.
        KeyCode::Home => {
            app.handle_home();
        }
        KeyCode::End => {
            app.handle_end();
        }

        // Delete key.
        KeyCode::Delete => {
            app.handle_delete();
        }

        // Ctrl+F: toggle zoom.
        KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if !app.panels.is_empty() {
                app.zoomed = !app.zoomed;
            }
        }

        // Character input.
        KeyCode::Char(c) => {
            app.handle_char(c);
        }

        // Backspace.
        KeyCode::Backspace => {
            app.handle_backspace();
        }

        // Up/Down: autocomplete → multiline input navigation → chat scroll.
        KeyCode::Up => {
            if app.autocomplete_active() {
                let suggestions = commands::autocomplete(app.current_input());
                if !suggestions.is_empty() {
                    let current = app.autocomplete_index.unwrap_or(0);
                    app.autocomplete_index = Some(if current == 0 {
                        suggestions.len() - 1
                    } else {
                        current - 1
                    });
                }
            } else {
                let width = app.active_input_width() as usize;
                let (row, _) = app.active_input().cursor_visual_position(width);
                if row > 0 {
                    app.handle_move_up(width);
                } else if let Some(panel) = app.focused_panel_mut() {
                    // scroll_offset = lines from bottom; adding moves up
                    panel.scroll_offset = panel.scroll_offset.saturating_add(1);
                }
            }
        }
        KeyCode::Down => {
            if app.autocomplete_active() {
                let suggestions = commands::autocomplete(app.current_input());
                if !suggestions.is_empty() {
                    let current = app.autocomplete_index.unwrap_or(suggestions.len().saturating_sub(1));
                    app.autocomplete_index = Some((current + 1) % suggestions.len());
                }
            } else {
                let width = app.active_input_width() as usize;
                let total_lines = app.active_input().visual_line_count(width);
                let (row, _) = app.active_input().cursor_visual_position(width);
                if row + 1 < total_lines {
                    app.handle_move_down(width);
                } else if let Some(panel) = app.focused_panel_mut() {
                    // scroll_offset = lines from bottom; subtracting moves down
                    panel.scroll_offset = panel.scroll_offset.saturating_sub(1);
                }
            }
        }
        KeyCode::PageUp => {
            if let Some(panel) = app.focused_panel_mut() {
                panel.scroll_offset = panel.scroll_offset.saturating_add(10);
            }
        }
        KeyCode::PageDown => {
            if let Some(panel) = app.focused_panel_mut() {
                panel.scroll_offset = panel.scroll_offset.saturating_sub(10);
            }
        }

        _ => {}
    }
}

/// Handle a parsed slash command.
async fn handle_command(
    app: &mut App,
    cmd: Command,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    match cmd {
        Command::Quit => {
            if app.input_focus == InputFocus::Panel {
                if let Some(panel) = app.focused_panel_mut() {
                    panel.chat_history.push(ChatMessage {
                        role: MessageRole::System,
                        content: "Press Esc to return to the global bar, then /q to quit."
                            .to_string(),
                    });
                }
            } else {
                app.should_quit = true;
            }
        }
        Command::Help => {
            let msg = ChatMessage {
                role: MessageRole::System,
                content: concat!(
                    "Available commands:\n",
                    "  /add <agent> [--image <img>] [--project <path>] [--branch <name>] [--name <name>]\n",
                    "                                Add a new agent panel\n",
                    "  /sandboxes                    Toggle sandbox sidebar\n",
                    "  /focus <n>                    Focus panel n (0-indexed)\n",
                    "  /close                        Close focused panel\n",
                    "  /kill [n]                     Kill sandbox & remove panel (0-indexed)\n",
                    "  /copy                         Copy panel content to clipboard\n",
                    "  /zoom                         Toggle panel zoom (Ctrl+F)\n",
                    "  /clear                        Clear chat history\n",
                    "  /env [KEY=VALUE]              Set/list panel env vars\n",
                    "  /reconnect                    Reconnect SSH terminal\n",
                    "  /branches                     List nanosb branches in project\n",
                    "  /mcp                          Toggle MCP sidebar\n",
                    "  /mcp list                     List MCP servers\n",
                    "  /mcp add <name> <cmd> [args]  Add MCP server\n",
                    "  /mcp remove <name>            Remove MCP server\n",
                    "  /mcp enable <name>            Enable MCP server\n",
                    "  /mcp disable <name>           Disable MCP server\n",
                    "  /gitsync [on|off|now]         Sync sandbox commits to local repo\n",
                    "  /open [tool]                  Open clone in external tool\n",
                    "  /quit                         Exit the TUI\n",
                    "  Config: Place sandbox.yml in project root for auto-start\n",
                )
                .to_string(),
            };
            if let Some(panel) = app.focused_panel_mut() {
                panel.chat_history.push(msg);
            } else {
                app.system_messages.push(msg);
            }
        }
        Command::Clear => {
            if let Some(panel) = app.focused_panel_mut() {
                panel.chat_history.clear();
            } else {
                app.system_messages.clear();
            }
        }
        Command::Close => {
            if !app.panels.is_empty() {
                let idx = app.focused_panel;
                app.panels.remove(idx);
                if app.panels.is_empty() {
                    app.focused_panel = 0;
                    app.focus_global();
                    app.zoomed = false;
                } else if app.focused_panel >= app.panels.len() {
                    app.focused_panel = app.panels.len() - 1;
                }
            } else {
                app.system_messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "No panels to close.".to_string(),
                });
            }
        }
        Command::Focus { panel } => {
            if panel < app.panels.len() {
                app.focused_panel = panel;
                app.focus_panel_input();
            } else {
                let msg = ChatMessage {
                    role: MessageRole::System,
                    content: format!("No panel {}. Use /add <agent> first.", panel),
                };
                if let Some(p) = app.focused_panel_mut() {
                    p.chat_history.push(msg);
                } else {
                    app.system_messages.push(msg);
                }
            }
        }
        Command::McpToggle => {
            app.show_mcp_sidebar = !app.show_mcp_sidebar;
        }
        Command::AddAgent { agent, image, project, branch, name } => {
            add_agent(app, &agent, image.as_deref(), project.as_deref(), branch.as_deref(), name.as_deref(), tx);
        }
        Command::Env { assignment } => {
            handle_env(app, assignment);
        }
        Command::Sandboxes => {
            app.show_sandbox_sidebar = !app.show_sandbox_sidebar;
            if app.show_sandbox_sidebar {
                app.refresh_sidebar_modified_files();
            }
        }
        Command::Reconnect => {
            let panel_idx = app.focused_panel;
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                // Drop existing SSH connection and reset URL tracking.
                panel.terminal = None;
                panel.terminal_handle = None;
                panel.opened_urls.clear();

                let ssh_info = if let Some(ref sb_arc) = panel.sandbox {
                    let sb = sb_arc.lock().await;
                    let port = sb.ssh_port();
                    let key = sb.ssh_key_path();
                    port.zip(key)
                } else {
                    None
                };

                if let Some((ssh_port, key_path)) = ssh_info {
                    let agent_name = panel.agent_name.clone();
                    let env = panel.env.clone();
                    let workdir = panel.project_mount.as_ref().map(|_| "/workspace".to_string());
                    let tx = tx.clone();
                    panel.chat_history.push(ChatMessage {
                        role: MessageRole::System,
                        content: "Reconnecting SSH terminal...".to_string(),
                    });
                    tokio::spawn(async move {
                        match super::terminal::connect_ssh(
                            ssh_port, key_path, 80, 24,
                            &agent_name, &env, workdir.as_deref(), panel_idx, tx.clone(),
                        ).await {
                            Ok(handle) => {
                                let _ = tx.send(AppEvent::SshConnected { panel_idx, handle });
                            }
                            Err(e) => {
                                let _ = tx.send(AppEvent::SshDisconnected {
                                    panel_idx,
                                    error: Some(format!("SSH reconnect failed: {}", e)),
                                });
                            }
                        }
                    });
                } else {
                    panel.chat_history.push(ChatMessage {
                        role: MessageRole::System,
                        content: "No sandbox running. Cannot reconnect SSH.".to_string(),
                    });
                }
            } else {
                app.system_messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "No panel focused. Use /add <agent> first.".to_string(),
                });
            }
        }
        Command::Kill { panel } => {
            let idx = match panel {
                Some(n) => n,
                None => app.focused_panel,
            };

            if idx >= app.panels.len() {
                let msg = ChatMessage {
                    role: MessageRole::System,
                    content: format!("No panel {}.", idx),
                };
                if let Some(p) = app.focused_panel_mut() {
                    p.chat_history.push(msg);
                } else {
                    app.system_messages.push(msg);
                }
            } else {
                // Teardown project mount before removing the panel.
                if let Some(mut pm) = app.panels[idx].project_mount.take() {
                    if let Err(e) = pm.teardown() {
                        eprintln!("Warning: project mount teardown failed: {}", e);
                    }
                }

                let sandbox_arc = app.panels[idx].sandbox.take();
                let agent_name = app.panels[idx].agent_name.clone();

                app.panels.remove(idx);
                if app.panels.is_empty() {
                    app.focused_panel = 0;
                    app.focus_global();
                    app.zoomed = false;
                } else if app.focused_panel >= app.panels.len() {
                    app.focused_panel = app.panels.len() - 1;
                }

                // Destroy the sandbox in the background.
                if let Some(sb_arc) = sandbox_arc {
                    tokio::spawn(async move {
                        match Arc::try_unwrap(sb_arc) {
                            Ok(mutex) => {
                                let sandbox = mutex.into_inner();
                                let _ = sandbox.destroy().await;
                            }
                            Err(arc) => {
                                let mut sb = arc.lock().await;
                                let _ = sb.stop().await;
                            }
                        }
                    });
                }

                let msg = ChatMessage {
                    role: MessageRole::System,
                    content: format!("Killed '{}'.", agent_name),
                };
                if let Some(p) = app.focused_panel_mut() {
                    p.chat_history.push(msg);
                } else {
                    app.system_messages.push(msg);
                }
            }
        }
        Command::McpList
        | Command::McpAdd { .. }
        | Command::McpRemove { .. }
        | Command::McpEnable { .. }
        | Command::McpDisable { .. } => {
            if app.panels.is_empty() {
                app.system_messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "MCP commands require an active panel. Use /add <agent> first."
                        .to_string(),
                });
            } else {
                match cmd {
                    Command::McpList => handle_mcp_list(app).await,
                    Command::McpAdd { name, command, args } => {
                        handle_mcp_add(app, &name, &command, &args).await;
                    }
                    Command::McpRemove { name } => handle_mcp_remove(app, &name).await,
                    Command::McpEnable { name } => handle_mcp_enable(app, &name).await,
                    Command::McpDisable { name } => handle_mcp_disable(app, &name).await,
                    _ => unreachable!(),
                }
            }
        }
        Command::Copy => {
            handle_copy(app);
        }
        Command::Zoom => {
            if !app.panels.is_empty() {
                app.zoomed = !app.zoomed;
            }
        }
        Command::Branches => {
            let project_dir = app.project_path.as_ref();
            if let Some(dir) = project_dir {
                let output = std::process::Command::new("git")
                    .args(["branch", "--list", "nanosb/*"])
                    .current_dir(dir)
                    .output();
                let msg = match output {
                    Ok(out) => {
                        let branches = String::from_utf8_lossy(&out.stdout);
                        if branches.trim().is_empty() {
                            "No nanosb branches found.".to_string()
                        } else {
                            format!("Nanosb branches:\n{}", branches)
                        }
                    }
                    Err(e) => format!("Failed to list branches: {}", e),
                };
                let chat_msg = ChatMessage {
                    role: MessageRole::System,
                    content: msg,
                };
                if let Some(panel) = app.focused_panel_mut() {
                    panel.chat_history.push(chat_msg);
                } else {
                    app.system_messages.push(chat_msg);
                }
            } else {
                let msg = ChatMessage {
                    role: MessageRole::System,
                    content: "No project configured. Use --project flag when launching nanosb.".to_string(),
                };
                if let Some(panel) = app.focused_panel_mut() {
                    panel.chat_history.push(msg);
                } else {
                    app.system_messages.push(msg);
                }
            }
        }
        Command::GitSync { action } => {
            let panel_idx = app.focused_panel;
            match action.as_deref() {
                None => {
                    // Show sync status
                    let auto = app.panels.get(panel_idx)
                        .and_then(|p| p.sync_override)
                        .unwrap_or(app.settings.gitsync.auto_sync);
                    let status_label = if auto { "ON (unsafe)" } else { "OFF (safe)" };
                    let has_branch = app.panels.get(panel_idx)
                        .and_then(|p| p.project_mount.as_ref())
                        .map(|pm| !pm.created_branches.is_empty())
                        .unwrap_or(false);
                    let branch_info = if has_branch {
                        app.panels.get(panel_idx)
                            .and_then(|p| p.project_mount.as_ref())
                            .and_then(|pm| pm.created_branches.first())
                            .map(|(_, b)| format!("Branch: {}", b))
                            .unwrap_or_default()
                    } else {
                        "No source branch created yet".to_string()
                    };
                    let msg = format!(
                        "Git sync: {}\nNotify on commit: {}\n{}",
                        status_label,
                        if app.settings.gitsync.notify_on_commit { "ON" } else { "OFF" },
                        branch_info,
                    );
                    if let Some(panel) = app.panels.get_mut(panel_idx) {
                        panel.chat_history.push(ChatMessage {
                            role: MessageRole::System,
                            content: msg,
                        });
                    }
                }
                Some("on") => {
                    if let Some(panel) = app.panels.get_mut(panel_idx) {
                        panel.sync_override = Some(true);
                        panel.chat_history.push(ChatMessage {
                            role: MessageRole::System,
                            content: "Auto-sync ENABLED for this panel.\n\
                                      WARNING: Agent commits will be fetched to your local branch automatically.\n\
                                      This can be unsafe — use /gitsync off to disable.".to_string(),
                        });
                        // Create source branch if deferred
                        if let Some(ref mut pm) = panel.project_mount {
                            if pm.created_branches.is_empty() {
                                if let Err(e) = pm.create_source_branch_and_fetch() {
                                    panel.chat_history.push(ChatMessage {
                                        role: MessageRole::System,
                                        content: format!("Failed to create source branch: {}", e),
                                    });
                                }
                            }
                        }
                    }
                }
                Some("off") => {
                    if let Some(panel) = app.panels.get_mut(panel_idx) {
                        panel.sync_override = Some(false);
                        panel.chat_history.push(ChatMessage {
                            role: MessageRole::System,
                            content: "Auto-sync DISABLED for this panel.".to_string(),
                        });
                    }
                }
                Some("now") => {
                    if let Some(panel) = app.panels.get_mut(panel_idx) {
                        if let Some(ref mut pm) = panel.project_mount {
                            // Create source branch if deferred
                            if pm.created_branches.is_empty() {
                                if let Err(e) = pm.create_source_branch_and_fetch() {
                                    panel.chat_history.push(ChatMessage {
                                        role: MessageRole::System,
                                        content: format!("Failed to create source branch: {}", e),
                                    });
                                    return;
                                }
                            }
                            // Fetch current state
                            if let Some(ref wt_base) = pm.worktree_base {
                                if let Some((source, branch)) = pm.created_branches.first() {
                                    let refspec = format!("{}:{}", branch, branch);
                                    let ok = std::process::Command::new("git")
                                        .args(["fetch", &wt_base.to_string_lossy(), &refspec, "--force"])
                                        .current_dir(source)
                                        .output()
                                        .map(|o| o.status.success())
                                        .unwrap_or(false);
                                    let msg = if ok {
                                        format!("Synced to branch '{}'.", branch)
                                    } else {
                                        "Sync failed. Check clone state.".to_string()
                                    };
                                    panel.chat_history.push(ChatMessage {
                                        role: MessageRole::System,
                                        content: msg,
                                    });
                                }
                            }
                        } else {
                            panel.chat_history.push(ChatMessage {
                                role: MessageRole::System,
                                content: "No project mount for this panel.".to_string(),
                            });
                        }
                    }
                }
                _ => {} // parse_gitsync already validates
            }
        }
        Command::Open { tool } => {
            let panel_idx = app.focused_panel;
            let clone_path = app.panels.get(panel_idx)
                .and_then(|p| p.project_mount.as_ref())
                .and_then(|pm| pm.worktree_base.clone());

            let clone_path = match clone_path {
                Some(p) => p,
                None => {
                    app.set_status_message("No project clone for this panel.");
                    return;
                }
            };

            // Use the explicit tool arg, or fall back to settings preference
            let editor_pref = tool.as_deref()
                .unwrap_or(&app.settings.tools.editor);

            // Handle custom command template
            if let Some(ref cmd_template) = app.settings.tools.custom_command {
                if editor_pref == "custom" || (editor_pref == "auto" && crate::settings::resolve_tool("auto").is_none()) {
                    let cmd = cmd_template.replace("{path}", &clone_path.to_string_lossy());
                    let parts: Vec<&str> = cmd.split_whitespace().collect();
                    if let Some((bin, args)) = parts.split_first() {
                        let _ = std::process::Command::new(bin)
                            .args(args)
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn();
                    }
                    app.set_status_message("Opened with custom command.");
                    return;
                }
            }

            let resolved = crate::settings::resolve_tool(editor_pref);

            match resolved {
                Some((binary, true)) => {
                    // TUI tool: send event to trigger suspend-and-launch in event loop
                    app.set_status_message(format!("Opening in {}...", binary));
                    let _ = tx.send(AppEvent::OpenTuiTool {
                        binary: binary.to_string(),
                        path: clone_path,
                    });
                }
                Some((binary, false)) => {
                    // GUI tool: fire-and-forget
                    let _ = std::process::Command::new(binary)
                        .arg(&clone_path)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn();
                    app.set_status_message(format!("Opened in {}.", binary));
                }
                None => {
                    // On macOS, try `open -a <AppName>` for known GUI apps
                    // whose shell command isn't on PATH.
                    #[cfg(target_os = "macos")]
                    if let Some(app_name) = crate::settings::macos_app_name(editor_pref) {
                        let ok = std::process::Command::new("open")
                            .args(["-a", app_name])
                            .arg(&clone_path)
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .status()
                            .map(|s| s.success())
                            .unwrap_or(false);
                        if ok {
                            app.set_status_message(format!("Opened in {}.", app_name));
                            return;
                        }
                    }

                    app.set_status_message(format!(
                        "No tool '{}' found. Install gitui, lazygit, or VS Code.",
                        editor_pref,
                    ));
                }
            }
        }
    }
}

/// Copy focused panel content to the system clipboard.
fn handle_copy(app: &mut App) {
    let panel = match app.focused_panel_mut() {
        Some(p) => p,
        None => {
            app.system_messages.push(ChatMessage {
                role: MessageRole::System,
                content: "No panel focused. Use /add <agent> first.".to_string(),
            });
            return;
        }
    };

    // Collect text to copy.
    let text = if panel.mode == PanelMode::Terminal {
        // Terminal mode: copy the vt100 screen buffer contents.
        panel
            .terminal
            .as_ref()
            .map(|t| t.screen().contents())
            .unwrap_or_default()
    } else {
        // Chat mode: copy chat history.
        panel
            .chat_history
            .iter()
            .map(|msg| match msg.role {
                MessageRole::User => format!("You: {}", msg.content),
                MessageRole::Agent => msg.content.clone(),
                MessageRole::System => format!("! {}", msg.content),
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    if text.is_empty() {
        panel.chat_history.push(ChatMessage {
            role: MessageRole::System,
            content: "Nothing to copy.".to_string(),
        });
        return;
    }

    // Write to system clipboard via platform command.
    let result = copy_to_clipboard(&text);
    // Re-borrow panel after the clipboard operation.
    if let Some(panel) = app.focused_panel_mut() {
        match result {
            Ok(()) => {
                panel.chat_history.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Copied {} chars to clipboard.", text.len()),
                });
            }
            Err(e) => {
                panel.chat_history.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Failed to copy: {}", e),
                });
            }
        }
    }
}

/// Write text to the system clipboard using platform-specific commands.
fn copy_to_clipboard(text: &str) -> std::result::Result<(), String> {
    use std::io::Write;
    use std::process::{Command as ProcessCommand, Stdio};

    #[cfg(target_os = "macos")]
    let mut child = ProcessCommand::new("pbcopy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("pbcopy: {}", e))?;

    #[cfg(target_os = "linux")]
    let mut child = ProcessCommand::new("xclip")
        .args(["-selection", "clipboard"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("xclip: {}", e))?;

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Err("Clipboard not supported on this platform".to_string());

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        child
            .stdin
            .as_mut()
            .ok_or("Failed to open stdin")?
            .write_all(text.as_bytes())
            .map_err(|e| format!("write: {}", e))?;

        let status = child.wait().map_err(|e| format!("wait: {}", e))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("exited with {}", status))
        }
    }
}

/// Required API key environment variables for known agents.
fn required_api_keys(agent: &str) -> Vec<(&'static str, bool)> {
    match agent {
        "claude" => vec![("ANTHROPIC_API_KEY", true)],
        "codex" => vec![("OPENAI_API_KEY", true)],
        "goose" => vec![
            ("OPENAI_API_KEY", false),
            ("ANTHROPIC_API_KEY", false),
        ],
        _ => vec![],
    }
}

/// Add a new agent panel, creating and starting a sandbox in the background.
fn add_agent(
    app: &mut App,
    agent: &str,
    image: Option<&str>,
    project: Option<&str>,
    branch: Option<&str>,
    name: Option<&str>,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    let image_name = match image {
        Some(img) => img.to_string(),
        None => format!("ghcr.io/devdone-labs/agents-registry/{}:latest", agent),
    };

    let mut panel = AgentPanel::new(agent);
    panel.chat_history.push(ChatMessage {
        role: MessageRole::System,
        content: format!("Launching {} (image: {})...", agent, image_name),
    });

    // Auto-detect API keys from host environment.
    for (key, _is_required) in &required_api_keys(agent) {
        if let Ok(val) = std::env::var(key) {
            panel.env.insert(key.to_string(), val);
        }
    }

    // Goose requires GOOSE_PROVIDER to be set — auto-detect from available keys.
    if agent == "goose" && !panel.env.contains_key("GOOSE_PROVIDER") {
        if panel.env.contains_key("ANTHROPIC_API_KEY") {
            panel.env.insert("GOOSE_PROVIDER".to_string(), "anthropic".to_string());
        } else if panel.env.contains_key("OPENAI_API_KEY") {
            panel.env.insert("GOOSE_PROVIDER".to_string(), "openai".to_string());
        }
    }

    app.panels.push(panel);
    let panel_idx = app.panels.len() - 1;
    app.focused_panel = panel_idx;
    app.show_welcome = false;
    app.focus_panel_input();

    // Build sandbox config.
    // Agent VMs need enough memory for the agent CLI + runtime overhead.
    let project_path = project
        .map(std::path::PathBuf::from)
        .or_else(|| app.project_path.clone());

    let sandbox_name = name
        .map(String::from)
        .unwrap_or_else(|| format!("tui-{}", agent));

    let mut builder = SandboxConfig::builder()
        .name(&sandbox_name)
        .image(&image_name)
        .memory_mb(1024);

    if let Some(ref pp) = project_path {
        builder = builder.project(pp, branch);
    }

    let mut config = builder.build();

    // Pass auto_sync setting to project config so sandbox creation
    // knows whether to use setup() or setup_deferred().
    if let Some(ref mut proj) = config.project {
        proj.auto_sync = app.settings.gitsync.auto_sync;
    }

    // Spawn sandbox creation in the background so the event loop stays responsive.
    let tx = tx.clone();
    tokio::spawn(async move {
        match Sandbox::create(config).await {
            Ok(mut sandbox) => {
                let short_id = sandbox.id()[..8.min(sandbox.id().len())].to_string();

                match sandbox.start().await {
                    Ok(()) => {
                        // Take the project mount from the sandbox so we can
                        // store it on the panel for teardown on kill.
                        let project_mount = sandbox.take_project_mount();
                        let sb = Arc::new(Mutex::new(sandbox));
                        let _ = tx.send(AppEvent::SandboxReady {
                            panel_idx,
                            sandbox: sb,
                            short_id,
                            project_mount,
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(AppEvent::SandboxFailed {
                            panel_idx,
                            error: format!("Failed to start sandbox: {}", e),
                        });
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(AppEvent::SandboxFailed {
                    panel_idx,
                    error: format!("Failed to create sandbox: {}", e),
                });
            }
        }
    });
}

/// Add an agent panel from a resolved SandboxConfig (from sandbox.yml).
fn add_agent_from_config(
    app: &mut App,
    key: &str,
    config: SandboxConfig,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    let display_name = config.name.clone();
    let image_name = config.image.clone();

    // Use the sandbox key (e.g. "claude", "codex") as agent_name so that
    // agent_cli_command() can resolve the correct startup command.
    let mut panel = AgentPanel::new(key);
    panel.display_name = Some(display_name.clone());
    panel.chat_history.push(ChatMessage {
        role: MessageRole::System,
        content: format!("Launching {} (image: {})...", display_name, image_name),
    });

    // Copy env vars from config to panel.
    for (k, v) in &config.env {
        panel.env.insert(k.clone(), v.clone());
    }

    // Auto-detect API keys from host environment (if not already in config env).
    for (api_key, _) in &required_api_keys(key) {
        if !panel.env.contains_key(*api_key) {
            if let Ok(val) = std::env::var(api_key) {
                panel.env.insert(api_key.to_string(), val);
            }
        }
    }

    app.panels.push(panel);
    let panel_idx = app.panels.len() - 1;
    app.focused_panel = panel_idx;
    app.show_welcome = false;
    app.focus_panel_input();

    let tx = tx.clone();
    tokio::spawn(async move {
        match Sandbox::create(config).await {
            Ok(mut sandbox) => {
                let short_id = sandbox.id()[..8.min(sandbox.id().len())].to_string();

                match sandbox.start().await {
                    Ok(()) => {
                        let project_mount = sandbox.take_project_mount();
                        let sb = Arc::new(Mutex::new(sandbox));
                        let _ = tx.send(AppEvent::SandboxReady {
                            panel_idx,
                            sandbox: sb,
                            short_id,
                            project_mount,
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(AppEvent::SandboxFailed {
                            panel_idx,
                            error: format!("Failed to start sandbox: {}", e),
                        });
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(AppEvent::SandboxFailed {
                    panel_idx,
                    error: format!("Failed to create sandbox: {}", e),
                });
            }
        }
    });
}

/// Handle a regular user message by streaming it to the sandbox.
fn handle_message(
    app: &mut App,
    msg: &str,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    let panel_idx = app.focused_panel;
    let panel = match app.panels.get_mut(panel_idx) {
        Some(p) => p,
        None => return,
    };

    // Check if the panel has a sandbox.
    let sandbox = match panel.sandbox.as_ref() {
        Some(sb) => Arc::clone(sb),
        None => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: "No sandbox attached to this panel.".to_string(),
            });
            return;
        }
    };

    // Don't send if already streaming.
    if panel.is_streaming {
        panel.chat_history.push(ChatMessage {
            role: MessageRole::System,
            content: "Agent is still processing. Please wait.".to_string(),
        });
        return;
    }

    panel.is_streaming = true;

    let agent_name = panel.agent_name.clone();
    let message = msg.to_string();
    let tx = tx.clone();

    // Forward API keys: merge host env with panel-specific env (panel takes priority).
    let mut env: HashMap<String, String> = [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
    ]
    .iter()
    .filter_map(|key| std::env::var(key).ok().map(|val| (key.to_string(), val)))
    .collect();
    env.extend(panel.env.clone());

    // Spawn a background task for the streaming send_message call.
    tokio::spawn(async move {
        let sb = sandbox.lock().await;
        let tx_cb = tx.clone();
        let cb_panel_idx = panel_idx;

        let result = sb
            .send_message(&message, &agent_name, "", &env, move |text, is_stderr| {
                let _ = tx_cb.send(AppEvent::AgentOutput(
                    cb_panel_idx,
                    text.to_string(),
                    is_stderr,
                ));
            })
            .await;

        match result {
            Ok(exit_code) => {
                let _ = tx.send(AppEvent::AgentDone(panel_idx, exit_code));
            }
            Err(e) => {
                let _ = tx.send(AppEvent::AgentOutput(
                    panel_idx,
                    format!("\nError: {}", e),
                    true,
                ));
                let _ = tx.send(AppEvent::AgentDone(panel_idx, -1));
            }
        }
    });
}

/// Handle `/env` — set or list panel environment variables.
fn handle_env(app: &mut App, assignment: Option<(String, String)>) {
    match assignment {
        None => {
            if let Some(panel) = app.focused_panel_mut() {
                if panel.env.is_empty() {
                    panel.chat_history.push(ChatMessage {
                        role: MessageRole::System,
                        content: "No environment variables set.\n\
                                  Use /env KEY=VALUE to set one."
                            .to_string(),
                    });
                } else {
                    let mut lines = vec!["Environment variables:".to_string()];
                    for (key, value) in &panel.env {
                        let masked = if value.len() > 8 {
                            format!("{}...{}", &value[..4], &value[value.len() - 4..])
                        } else {
                            "****".to_string()
                        };
                        lines.push(format!("  {}={}", key, masked));
                    }
                    panel.chat_history.push(ChatMessage {
                        role: MessageRole::System,
                        content: lines.join("\n"),
                    });
                }
            } else {
                app.system_messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "No panel focused. Use /add <agent> first.".to_string(),
                });
            }
        }
        Some((key, value)) => {
            if let Some(panel) = app.focused_panel_mut() {
                panel.env.insert(key.clone(), value);
                panel.chat_history.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Set {}.", key),
                });
            } else {
                app.system_messages.push(ChatMessage {
                    role: MessageRole::System,
                    content: "No panel focused. Use /add <agent> first.".to_string(),
                });
            }
        }
    }
}

/// Handle `/mcp list` — list MCP servers in the focused panel's sandbox.
async fn handle_mcp_list(app: &mut App) {
    let panel = match app.focused_panel_mut() {
        Some(p) => p,
        None => return,
    };

    let sandbox = match panel.sandbox.as_ref() {
        Some(sb) => Arc::clone(sb),
        None => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: "No sandbox attached to this panel.".to_string(),
            });
            return;
        }
    };

    let sb = sandbox.lock().await;
    match sb.list_mcp_servers().await {
        Ok(servers) => {
            if servers.is_empty() {
                panel.chat_history.push(ChatMessage {
                    role: MessageRole::System,
                    content: "No MCP servers configured.".to_string(),
                });
            } else {
                let mut lines = Vec::new();
                lines.push("MCP Servers:".to_string());
                for (name, cfg) in &servers {
                    let status = if cfg.enabled { "enabled" } else { "disabled" };
                    lines.push(format!(
                        "  {} [{}] - {} {}",
                        name,
                        status,
                        cfg.command,
                        cfg.args.join(" "),
                    ));
                }
                panel.chat_history.push(ChatMessage {
                    role: MessageRole::System,
                    content: lines.join("\n"),
                });
            }
        }
        Err(e) => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: format!("Failed to list MCP servers: {}", e),
            });
        }
    }
}

/// Handle `/mcp add <name> <command> [args]`.
async fn handle_mcp_add(app: &mut App, name: &str, command: &str, args: &[String]) {
    let panel = match app.focused_panel_mut() {
        Some(p) => p,
        None => return,
    };

    let sandbox = match panel.sandbox.as_ref() {
        Some(sb) => Arc::clone(sb),
        None => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: "No sandbox attached to this panel.".to_string(),
            });
            return;
        }
    };

    let config = McpServerConfig {
        command: command.to_string(),
        args: args.to_vec(),
        env: HashMap::new(),
        enabled: true,
    };

    let sb = sandbox.lock().await;
    match sb.add_mcp_server(name, config).await {
        Ok(()) => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: format!("MCP server '{}' added.", name),
            });
        }
        Err(e) => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: format!("Failed to add MCP server '{}': {}", name, e),
            });
        }
    }
}

/// Handle `/mcp remove <name>`.
async fn handle_mcp_remove(app: &mut App, name: &str) {
    let panel = match app.focused_panel_mut() {
        Some(p) => p,
        None => return,
    };

    let sandbox = match panel.sandbox.as_ref() {
        Some(sb) => Arc::clone(sb),
        None => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: "No sandbox attached to this panel.".to_string(),
            });
            return;
        }
    };

    let sb = sandbox.lock().await;
    match sb.remove_mcp_server(name).await {
        Ok(()) => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: format!("MCP server '{}' removed.", name),
            });
        }
        Err(e) => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: format!("Failed to remove MCP server '{}': {}", name, e),
            });
        }
    }
}

/// Handle `/mcp enable <name>`.
async fn handle_mcp_enable(app: &mut App, name: &str) {
    let panel = match app.focused_panel_mut() {
        Some(p) => p,
        None => return,
    };

    let sandbox = match panel.sandbox.as_ref() {
        Some(sb) => Arc::clone(sb),
        None => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: "No sandbox attached to this panel.".to_string(),
            });
            return;
        }
    };

    let sb = sandbox.lock().await;
    match sb.enable_mcp_server(name).await {
        Ok(()) => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: format!("MCP server '{}' enabled.", name),
            });
        }
        Err(e) => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: format!("Failed to enable MCP server '{}': {}", name, e),
            });
        }
    }
}

/// Handle `/mcp disable <name>`.
async fn handle_mcp_disable(app: &mut App, name: &str) {
    let panel = match app.focused_panel_mut() {
        Some(p) => p,
        None => return,
    };

    let sandbox = match panel.sandbox.as_ref() {
        Some(sb) => Arc::clone(sb),
        None => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: "No sandbox attached to this panel.".to_string(),
            });
            return;
        }
    };

    let sb = sandbox.lock().await;
    match sb.disable_mcp_server(name).await {
        Ok(()) => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: format!("MCP server '{}' disabled.", name),
            });
        }
        Err(e) => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: format!("Failed to disable MCP server '{}': {}", name, e),
            });
        }
    }
}
