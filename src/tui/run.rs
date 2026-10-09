//! Main event loop for the TUI.

use std::collections::HashMap;
use std::io::{self, IsTerminal};
use std::sync::Arc;
use std::time::Duration;

use ratatui::crossterm::cursor::SetCursorStyle;
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as CrosstermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::{mpsc, Mutex};

use sandbox::Sandbox;
use sandbox::{AgentSandboxConfig, SandboxConfig};

use super::app::{
    AgentPanel, App, ChatMessage, InputFocus, MessageRole, MouseSelection, PanelMode,
    SidebarFilesTab, SubmitResult, SYSTEM_POPUP_TICKS_DEFAULT,
};
use super::commands::{self, Command};
use super::event::{spawn_terminal_event_reader, AppEvent};
use super::renderer;

/// Cross-platform stderr redirection helpers.
///
/// On Unix, native C libraries (libkrun, gvproxy) write to stderr via fprintf()
/// which bypasses Rust's logging and corrupts the ratatui alternate screen.
/// We redirect stderr to /dev/null while the TUI is active.
mod stderr_redirect {
    pub type SavedStderr = i32;

    pub fn save_and_redirect() -> SavedStderr {
        use std::io;
        use std::os::fd::AsRawFd;
        let saved = unsafe { libc::dup(io::stderr().as_raw_fd()) };
        let dev_null = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY) };
        if dev_null >= 0 {
            unsafe {
                libc::dup2(dev_null, libc::STDERR_FILENO);
                libc::close(dev_null);
            }
        }
        saved
    }

    pub fn restore(saved: SavedStderr) {
        if saved >= 0 {
            unsafe {
                libc::dup2(saved, libc::STDERR_FILENO);
            }
        }
    }

    pub fn redirect_to_null() {
        let dev_null = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY) };
        if dev_null >= 0 {
            unsafe {
                libc::dup2(dev_null, libc::STDERR_FILENO);
                libc::close(dev_null);
            }
        }
    }

    pub fn restore_and_close(saved: SavedStderr) {
        if saved >= 0 {
            unsafe {
                libc::dup2(saved, libc::STDERR_FILENO);
                libc::close(saved);
            }
        }
    }
}

/// Run the TUI application.
///
/// This enables raw mode, enters the alternate screen, and runs the main
/// event loop. On exit (or error) it restores the terminal.
#[derive(Debug, Clone)]
pub enum SessionStartMode {
    Fresh,
    ResumeLatest,
    ResumeById(String),
}

/// Result of processing a single [`AppEvent`] in the headless event core.
pub enum EventOutcome {
    /// Keep looping.
    Continue,
    /// The application should exit.
    Quit,
    /// A TUI tool must be launched with the terminal suspended. The caller
    /// (which owns the real terminal) performs the suspend/run/resume.
    RunTuiTool {
        /// Binary name of the tool to launch.
        binary: String,
        /// Path to the clone directory to open.
        path: std::path::PathBuf,
    },
}

pub async fn run_tui(
    project_path: Option<std::path::PathBuf>,
    sandbox_configs: Vec<(String, sandbox::AgentSandboxConfig)>,
    session_start: SessionStartMode,
    runtime_env_pool: HashMap<String, String>,
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
    let validation = sandbox::validation::validate_runtime_prerequisites_detailed().await;

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

    // Resolve startup behavior before entering the alternate screen.
    let (resume_session_data, resumed_session_id) = if let Some(ref pp) = project_path {
        match &session_start {
            SessionStartMode::Fresh => (None, None),
            SessionStartMode::ResumeLatest => {
                if let Some(entry) = sandbox::session::Session::list(pp).into_iter().next() {
                    let issues = entry.session.validate();
                    if issues.iter().any(|i| !i.recoverable) {
                        eprintln!("Latest session has unrecoverable issues. Starting fresh.");
                        for issue in issues {
                            eprintln!("  - {}", issue);
                        }
                        (None, None)
                    } else {
                        if !issues.is_empty() {
                            eprintln!("Resuming latest session with warnings:");
                            for issue in issues {
                                eprintln!("  - {}", issue);
                            }
                        } else {
                            eprintln!("Resuming latest session.");
                        }
                        (Some(entry.session), Some(entry.id))
                    }
                } else {
                    eprintln!("No previous session found. Starting fresh.");
                    (None, None)
                }
            }
            SessionStartMode::ResumeById(session_id) => {
                if let Some(session) = sandbox::session::Session::load_by_id(pp, session_id) {
                    let issues = session.validate();
                    if issues.iter().any(|i| !i.recoverable) {
                        eprintln!(
                            "Session '{}' has unrecoverable issues. Starting fresh.",
                            session_id
                        );
                        for issue in issues {
                            eprintln!("  - {}", issue);
                        }
                        (None, None)
                    } else {
                        if !issues.is_empty() {
                            eprintln!("Resuming session '{}' with warnings:", session_id);
                            for issue in issues {
                                eprintln!("  - {}", issue);
                            }
                        } else {
                            eprintln!("Resuming session '{}'.", session_id);
                        }
                        (Some(session), Some(session_id.clone()))
                    }
                } else {
                    eprintln!(
                        "Session '{}' not found for this project. Starting fresh.",
                        session_id
                    );
                    let available: Vec<String> = sandbox::session::Session::list(pp)
                        .into_iter()
                        .map(|entry| entry.id)
                        .collect();
                    if !available.is_empty() {
                        eprintln!("Available session IDs: {}", available.join(", "));
                    }
                    (None, None)
                }
            }
        }
    } else {
        match session_start {
            SessionStartMode::Fresh => {}
            _ => eprintln!("No project path available; cannot resume session. Starting fresh."),
        }
        (None, None)
    };

    // Redirect stderr to /dev/null before entering the alternate screen.
    // Native C libraries (libkrun, gvproxy) write to stderr via fprintf()
    // which bypasses Rust's logging. Without this redirect those writes
    // corrupt the ratatui alternate screen or cause panics when the
    // terminal buffer fills up (EAGAIN / os error 35).
    let saved_stderr = stderr_redirect::save_and_redirect();

    // Set up terminal.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        SetCursorStyle::SteadyBar
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Install a panic hook that restores the terminal before printing
    // the panic message. Without this, panics corrupt the alternate screen.
    let original_hook = std::panic::take_hook();
    let saved_stderr_for_hook = saved_stderr;
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            SetCursorStyle::DefaultUserShape,
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        // Restore stderr so the panic message is visible.
        stderr_redirect::restore(saved_stderr_for_hook);
        original_hook(info);
    }));

    // Create app state.
    let mut app = App::new();
    app.project_path = project_path.clone();
    app.runtime_env_pool = runtime_env_pool;

    // Auto-register the current project in the project registry.
    if let Some(ref pp) = project_path {
        let mut registry = sandbox::ProjectRegistry::load();
        registry.register(pp);
    }

    // Create shared image manager so all sandboxes coordinate pulls
    // (prevents concurrent downloads of the same image layers).
    app.image_manager = sandbox::ImageManager::with_default_cache()
        .ok()
        .map(Arc::new);

    // Try to load agents registry from well-known locations.
    app.registry = load_agents_registry();

    // Create the event channel.
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

    // Spawn terminal event reader.
    spawn_terminal_event_reader(tx.clone());

    // Resolve agent definitions + skills from registry before launching.
    let mut resolved_configs: Vec<(String, AgentSandboxConfig)> = Vec::new();
    for (key, mut config) in sandbox_configs {
        match (&app.registry, config.agent.as_ref()) {
            (Some(registry), Some(agent_name)) => {
                match registry.resolve_full(agent_name, &config.skills) {
                    Ok(resolved) => merge_resolved_agent_into_config(&mut config, resolved),
                    Err(e) => {
                        eprintln!("Warning: failed to resolve agent '{}': {}", agent_name, e);
                    }
                }
            }
            (Some(registry), None) if !config.skills.is_empty() => {
                match registry.resolve_skills_only(&config.skills) {
                    Ok(resolved) => merge_resolved_agent_into_config(&mut config, resolved),
                    Err(e) => {
                        eprintln!("Warning: failed to resolve sandbox skills: {}", e);
                    }
                }
            }
            (None, None) if !config.skills.is_empty() => {
                eprintln!(
                    "Warning: sandbox lists skills but no agents registry was found. \
Set NANOSB_REGISTRY_PATH or install the registry at ~/.nanosandbox/agents-registry/ \
(sibling ../agents-registry is also checked from the current directory)."
                );
            }
            _ => {}
        }
        resolved_configs.push((key, config));
    }

    // Either resume a previous session or start fresh sandboxes.
    if let Some(ref session) = resume_session_data {
        if let Err(e) = resume_session(&mut app, session, &tx) {
            eprintln!("Warning: failed to resume session: {}", e);
        }
    } else {
        // Auto-start sandboxes from config file.
        for (key, config) in resolved_configs {
            add_agent_from_config(&mut app, &key, config, &tx);
        }
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
    //
    while let Some(event) = rx.recv().await {
        match handle_event(&mut app, event, &tx).await {
            EventOutcome::Quit => break,
            EventOutcome::RunTuiTool { binary, path } => {
                // Suspend TUI: leave alternate screen, disable raw mode
                let _ = disable_raw_mode();
                let _ = execute!(
                    terminal.backend_mut(),
                    SetCursorStyle::DefaultUserShape,
                    DisableMouseCapture,
                    LeaveAlternateScreen
                );

                // Restore stderr so the tool can use it
                stderr_redirect::restore(saved_stderr);

                // Build tool-specific arguments
                let path_str = path.to_string_lossy().to_string();
                let mut cmd = std::process::Command::new(&binary);
                match binary.as_str() {
                    "gitui" => {
                        cmd.args(["-d", &path_str]);
                    }
                    "lazygit" => {
                        cmd.args(["-p", &path_str]);
                    }
                    "tig" => {
                        cmd.current_dir(&path);
                    }
                    _ => {
                        cmd.arg(&path);
                    }
                };
                // Block until tool exits
                let _ = cmd.status();

                // Redirect stderr back to /dev/null
                stderr_redirect::redirect_to_null();

                // Resume TUI: enter alternate screen, enable raw mode
                let _ = enable_raw_mode();
                let _ = execute!(
                    terminal.backend_mut(),
                    EnterAlternateScreen,
                    EnableMouseCapture,
                    EnableBracketedPaste,
                    SetCursorStyle::SteadyBar
                );
                terminal.clear()?;
            }
            EventOutcome::Continue => {}
        }
        if app.should_quit {
            break;
        }

        // Re-render after every event.
        terminal.draw(|frame| renderer::render(frame, &mut app))?;
    }

    // Restore terminal before cleanup so the user sees progress messages.
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        SetCursorStyle::DefaultUserShape,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    // Restore stderr so cleanup log messages are visible.
    stderr_redirect::restore_and_close(saved_stderr);

    // Determine whether to suspend (preserve session) or fully teardown.
    // Suspend when: project is mounted AND /quit (not /destroy).
    let should_suspend = app.project_path.is_some() && !app.destroy_on_quit;

    if should_suspend {
        // Suspend session: auto-commit + sync but keep clones alive.
        eprintln!("Suspending session...");
        for panel in &mut app.panels {
            if let Some(ref mut pm) = panel.project_mount {
                let _ = pm.suspend();
            }
        }

        // Save session state for later resume.
        if let Some(ref project_path) = app.project_path {
            let sandbox_yml_content = sandbox::find_sandbox_file(project_path)
                .and_then(|p| std::fs::read_to_string(p).ok())
                .unwrap_or_default();

            match build_session_from_app(&app, project_path, &sandbox_yml_content) {
                Ok(session) => match session.save_with_id(resumed_session_id.as_deref()) {
                    Ok(session_id) => {
                        eprintln!(
                            "Session saved (id: {}). Use `nanosb -r` to resume latest or `nanosb --session {}`.",
                            session_id, session_id
                        );
                    }
                    Err(e) => {
                        eprintln!("Warning: failed to save session: {}", e);
                    }
                },
                Err(e) => {
                    eprintln!("Warning: failed to build session state: {}", e);
                }
            }
        }
    } else {
        // Full teardown: auto-commit + sync + remove clones.
        for panel in &mut app.panels {
            if let Some(mut pm) = panel.project_mount.take() {
                let _ = pm.teardown();
            }
        }

        // Delete session file if /destroy was used.
        if app.destroy_on_quit {
            if let Some(ref project_path) = app.project_path {
                let _ = sandbox::session::Session::delete(project_path, true);
                eprintln!("Session destroyed.");
            }
        }
    }

    // Stop/destroy all sandbox VMs.
    let sandbox_count = app.panels.iter().filter(|p| p.sandbox().is_some()).count();
    if sandbox_count > 0 {
        eprintln!("Shutting down {} sandbox(es)...", sandbox_count);

        let mut handles = Vec::new();
        for panel in &mut app.panels {
            if let Some(sb_arc) = panel.take_sandbox() {
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

    // Windows safety net: even with spawn_blocking on the HvSocket SSE reader,
    // a blocking-pool thread parked in recv() cannot be cancelled. The tokio
    // runtime drop will skip it (because blocking-pool threads are detached),
    // but belt-and-suspenders — explicitly exit so the process never lingers
    // after /quit or /destroy.
    Ok(())
}

/// Process a single [`AppEvent`], mutating `app` and possibly spawning work
/// via `tx`. This is the headless event core: it performs NO terminal I/O.
/// The one terminal-coupled event ([`AppEvent::OpenTuiTool`]) is returned to
/// the caller as [`EventOutcome::RunTuiTool`] instead of being executed here.
pub async fn handle_event(
    app: &mut App,
    event: AppEvent,
    tx: &mpsc::UnboundedSender<AppEvent>,
) -> EventOutcome {
    match event {
        AppEvent::Terminal(crossterm_event) => {
            match crossterm_event {
                CrosstermEvent::Key(key) if key.kind == KeyEventKind::Press => {
                    // Ctrl+C with active selection → copy to clipboard
                    // instead of forwarding/clearing.
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                        && app
                            .mouse_selection
                            .as_ref()
                            .is_some_and(|s| !s.dragging && s.start != s.end)
                    {
                        if let Some(sel) = app.mouse_selection.as_ref() {
                            let (start, end) = sel.normalized();
                            if let Some(panel) = app.panels.get(sel.panel_idx) {
                                if let Some(ref term) = panel.terminal {
                                    let text = term
                                        .screen()
                                        .contents_between(start.0, start.1, end.0, end.1);
                                    if !text.is_empty() {
                                        let _ = copy_to_clipboard(&text);
                                        app.set_status_message(format!(
                                            "Copied {} chars to clipboard.",
                                            text.len()
                                        ));
                                    }
                                }
                            }
                        }
                        app.mouse_selection = None;
                    } else {
                        // Clear mouse selection on any other keypress.
                        app.mouse_selection = None;
                        handle_key_event(app, key, tx).await;
                    }
                }
                CrosstermEvent::Key(_) => {
                    // Ignore Release / Repeat events.
                }
                CrosstermEvent::Mouse(mouse) => {
                    handle_mouse_event(app, mouse);
                }
                CrosstermEvent::Paste(text) => {
                    tracing::info!(
                        text_len = text.len(),
                        focused_panel = app.focused_panel,
                        "Received terminal paste event"
                    );
                    handle_paste_event(app, text, tx);
                }
                CrosstermEvent::Resize(_cols, _rows) => {
                    // ratatui picks up new size on next draw();
                    // render_panel() detects the delta and propagates
                    // to vt100 parser + SSH PTY.
                }
                CrosstermEvent::FocusGained => {}
                CrosstermEvent::FocusLost => {}
            }
        }
        AppEvent::SandboxCreating { panel_idx, message } => {
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                panel.loading_message = Some(message);
            }
        }
        AppEvent::SupervisorReady {
            panel_idx,
            name,
            short_id,
            project_mount,
            exec_pty,
        } => {
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                panel.backend = None;
                panel.set_supervisor(name.clone());
                panel.sandbox_id_short = short_id;
                if project_mount.is_some() {
                    panel.project_mount = project_mount;
                }
                panel.exec_pty = exec_pty.clone();
                panel.loading_message = Some("Attaching to supervised sandbox...".into());
                panel.mode = PanelMode::Loading;
            }

            let (pty_cols, pty_rows) = {
                let term_size = ratatui::crossterm::terminal::size().unwrap_or((160, 40));
                let has_sidebar = app.show_mcp_sidebar || app.show_sandbox_sidebar;
                super::grid::estimate_panel_inner_size(
                    term_size.0,
                    term_size.1,
                    app.visible_panel_count(),
                    has_sidebar,
                    app.zoomed,
                )
            };

            let tx = tx.clone();
            tokio::spawn(async move {
                let max_attempts = 10;
                let mut last_err = String::new();
                for attempt in 1..=max_attempts {
                    let delay = if attempt == 1 { 300 } else { 1000 };
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    let result = match exec_pty.clone() {
                        Some((sock, program, args)) => {
                            super::terminal::connect_exec_pty(
                                sock, program, args, pty_cols, pty_rows, panel_idx, tx.clone(),
                            )
                            .await
                        }
                        None => {
                            super::terminal::connect_console(
                                name.clone(),
                                pty_cols,
                                pty_rows,
                                panel_idx,
                                tx.clone(),
                            )
                            .await
                        }
                    };
                    match result {
                        Ok(handle) => {
                            let _ = tx.send(AppEvent::SshConnected { panel_idx, handle });
                            return;
                        }
                        Err(e) => {
                            last_err = e;
                        }
                    }
                }
                let _ = tx.send(AppEvent::SshDisconnected {
                    panel_idx,
                    error: Some(format!(
                        "Console attach failed after {} attempts: {}",
                        max_attempts, last_err
                    )),
                });
            });
        }
        AppEvent::SandboxFailed { panel_idx, error } => {
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                panel.loading_error = Some(error);
            }
        }
        AppEvent::SshConnected { panel_idx, handle } => {
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                let was_reconnecting = panel.reconnecting;
                if was_reconnecting && panel.terminal.is_some() {
                    // Reconnect: keep the existing terminal screen intact
                    // so the user doesn't see any flicker. Only attach
                    // the new SSH handle.
                    panel.terminal_handle = Some(handle);
                } else {
                    // Fresh connection: create new terminal.
                    let (cols, rows) = panel.last_terminal_size;
                    panel.terminal = Some(super::terminal::SshTerminal::new(cols, rows));
                    panel.terminal_handle = Some(handle);
                }
                panel.mode = if panel.auto_mode {
                    PanelMode::Headless
                } else {
                    PanelMode::Terminal
                };
                panel.reconnecting = false;
                panel.loading_message = None;
                panel.loading_error = None;
            }
        }
        AppEvent::TerminalData { panel_idx, data } => {
            // Clear selection if terminal content changes in the selected panel
            // (but not while user is actively dragging).
            if app
                .mouse_selection
                .as_ref()
                .is_some_and(|s| s.panel_idx == panel_idx && !s.dragging)
            {
                app.mouse_selection = None;
            }
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                if panel.mode == PanelMode::Headless {
                    // Headless: parse NDJSON lines from raw SSH bytes.
                    if let Some(ref mut hs) = panel.headless_state {
                        parse_headless_data(hs, &data);
                    }
                    // Still feed vt100 for /terminal fallback + URL extraction.
                    if let Some(ref mut term) = panel.terminal {
                        term.process_bytes(&data);

                        let urls = super::terminal::extract_urls_from_screen(term.screen());
                        let now = std::time::Instant::now();
                        for url in urls {
                            if !super::terminal::is_auth_url(&url) {
                                continue;
                            }
                            if !url.contains('?') && !super::terminal::is_device_code_url(&url)
                            {
                                continue;
                            }
                            let key = super::terminal::url_dedup_key(&url);
                            if panel.opened_urls.contains(&key) {
                                continue;
                            }
                            panel
                                .pending_urls
                                .entry(key)
                                .and_modify(|(existing_url, ts)| {
                                    if url.len() > existing_url.len() {
                                        *existing_url = url.clone();
                                        *ts = now;
                                    }
                                })
                                .or_insert((url, now));
                        }
                    }
                } else if let Some(ref mut term) = panel.terminal {
                    term.process_bytes(&data);

                    // Extract URLs from the parsed vt100 screen, then
                    // validate the hostname.  TUI apps (ink.js) re-render
                    // the screen, sometimes garbling text — host validation
                    // rejects those broken URLs.  The first clean read is
                    // buffered for 2s (keeping the longest per host+path),
                    // then opened once.
                    let urls = super::terminal::extract_urls_from_screen(term.screen());
                    let now = std::time::Instant::now();
                    for url in urls {
                        if !super::terminal::is_auth_url(&url) {
                            continue;
                        }
                        // OAuth authorize URLs always have query parameters
                        // (?client_id=...).  Truncated URLs from partial screen
                        // renders won't have reached the '?' yet — skip them.
                        // Exception: device-code flow URLs (e.g. github.com/login/device,
                        // cursor.com/loginlink) are complete without query params.
                        if !url.contains('?') && !super::terminal::is_device_code_url(&url) {
                            continue;
                        }
                        let key = super::terminal::url_dedup_key(&url);
                        if panel.opened_urls.contains(&key) {
                            continue;
                        }
                        // Keep the longest valid URL seen for each dedup key.
                        // Reset the debounce timer when the URL grows so we
                        // wait for the screen to stabilise after a re-render.
                        panel
                            .pending_urls
                            .entry(key)
                            .and_modify(|(existing_url, ts)| {
                                if url.len() > existing_url.len() {
                                    *existing_url = url.clone();
                                    *ts = now;
                                }
                            })
                            .or_insert((url, now));
                    }
                }
            }
        }
        AppEvent::SshDisconnected { panel_idx, error } => {
            // Check if this is a reconnect attempt that failed.
            let is_reconnecting = app.panels.get(panel_idx).is_some_and(|p| p.reconnecting);
            let is_headless = app
                .panels
                .get(panel_idx)
                .is_some_and(|p| p.mode == PanelMode::Headless);

            if is_reconnecting {
                // Reconnect failed: revert to loading screen with error.
                if let Some(panel) = app.panels.get_mut(panel_idx) {
                    panel.terminal = None;
                    panel.terminal_handle = None;
                    panel.mode = PanelMode::Loading;
                    panel.loading_error = error.map(|e| format!("Reconnect failed: {}", e));
                    panel.reconnecting = false;
                    panel.loading_tick = 0;
                }
            } else if is_headless {
                // Headless panel: keep panel visible so user can read output.
                // Mark the headless state as completed/error and clean up SSH resources.
                if let Some(panel) = app.panels.get_mut(panel_idx) {
                    // Only update status if not already marked completed/error
                    // (ExitStatus + Eof/Close both fire SshDisconnected).
                    if let Some(ref mut hs) = panel.headless_state {
                        if hs.status != "completed" && hs.status != "error" {
                            if let Some(ref err) = error {
                                hs.agent_text.push_str(&format!("\n[process] {}\n", err));
                                hs.finish("error");
                            } else {
                                hs.finish("completed");
                            }
                        }
                    }
                    // Drop SSH handle but keep the panel.
                    panel.terminal_handle = None;

                    let name = panel.agent_name.clone();
                    let msg = if let Some(ref err) = error {
                        format!("'{}' headless agent exited: {}", name, err)
                    } else {
                        format!("'{}' headless agent completed.", name)
                    };
                    app.set_status_message(msg);
                }
            } else {
                // Genuine disconnect: kill sandbox and close panel.
                if let Some((name, sandbox_arc, supervisor_name)) =
                    kill_panel_at(app, panel_idx)
                {
                    if let Some(sb) = sandbox_arc {
                        spawn_sandbox_destroy(sb);
                    }
                    if let Some(sname) = supervisor_name {
                        spawn_supervisor_stop(sname);
                    }

                    let msg = if let Some(err) = error {
                        format!("'{}' disconnected: {}", name, err)
                    } else {
                        format!("'{}' session ended.", name)
                    };
                    app.set_status_message(msg);
                }
            }
        }
        AppEvent::Tick => {
            // Increment loading animation counter and tick down panel notifications.
            for panel in app.panels.iter_mut() {
                if panel.mode == PanelMode::Loading {
                    panel.loading_tick = panel.loading_tick.wrapping_add(1);
                }
                if let Some((_, _, ref mut ticks)) = panel.notification {
                    *ticks = ticks.saturating_sub(1);
                    if *ticks == 0 {
                        panel.notification = None;
                    }
                }
            }

            // Flush pending URLs whose 2s debounce window has elapsed.
            let now = std::time::Instant::now();
            let debounce = std::time::Duration::from_secs(2);
            for panel in app.panels.iter_mut() {
                let ready: Vec<String> = panel
                    .pending_urls
                    .iter()
                    .filter(|(_key, (_url, first_seen))| {
                        now.duration_since(*first_seen) >= debounce
                    })
                    .map(|(key, _)| key.clone())
                    .collect();
                for key in ready {
                    if let Some((url, _)) = panel.pending_urls.remove(&key) {
                        tracing::debug!(
                            panel = %panel.agent_name,
                            dedup_key = %key,
                            url = %url,
                            "Auth URL debounce elapsed; opening in host browser"
                        );
                        panel.opened_urls.insert(key);
                        tracing::debug!(
                            panel = %panel.agent_name,
                            url = %url,
                            "Opening auth URL in host browser"
                        );
                        super::terminal::open_url_in_browser(&url);
                    }
                }
            }

            // Tick down temporary status message.
            if let Some((_, ref mut ticks)) = app.status_message {
                *ticks = ticks.saturating_sub(1);
                if *ticks == 0 {
                    app.status_message = None;
                }
            }

            // Tick down auto-dismiss system message popup.
            if let Some(ref mut ticks) = app.system_message_ticks {
                *ticks = ticks.saturating_sub(1);
                if *ticks == 0 {
                    app.system_messages.clear();
                    app.system_message_ticks = None;
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
            return EventOutcome::RunTuiTool { binary, path };
        }
        AppEvent::UploadStarted {
            panel_idx,
            filename,
        } => {
            let msg = format!("Uploading {}...", filename);
            // Show immediately; stays until replaced by Complete/Failed.
            // 120 ticks × 250ms = 30s (generous timeout, replaced on completion).
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                panel.notification = Some((msg, false, 120));
            }
        }
        AppEvent::UploadComplete {
            panel_idx,
            filename,
            remote_path,
            size,
        } => {
            let msg = format!(
                "Uploaded {} ({}) -> {}",
                filename,
                super::upload::format_size(size),
                remote_path,
            );
            // Show overlay notification on the panel (replaces previous, auto-dismisses).
            // 16 ticks × 250ms = 4s.
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                panel.notification = Some((msg, false, 16));
            }
        }
        AppEvent::UploadFailed { panel_idx, error } => {
            let msg = format!("Upload failed: {}", error);
            // 24 ticks × 250ms = 6s (errors stay longer).
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                panel.notification = Some((msg, true, 24));
            }
        }
    }

    if app.should_quit {
        EventOutcome::Quit
    } else {
        EventOutcome::Continue
    }
}

/// Print validation results as a checklist.
fn print_validation_results(validation: &sandbox::validation::ValidationResult) {
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
        let checks = [
            "Architecture",
            "libkrun Library",
            "Hypervisor.framework",
            "gvproxy",
        ];
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

/// Kill a panel's sandbox, teardown its project mount, and remove it from the panel list.
/// Returns the agent name and optional sandbox Arc for background destruction.
fn kill_panel_at(
    app: &mut App,
    idx: usize,
) -> Option<(String, Option<Arc<Mutex<Sandbox>>>, Option<String>)> {
    if idx >= app.panels.len() {
        return None;
    }

    // Teardown project mount.
    if let Some(mut pm) = app.panels[idx].project_mount.take() {
        let _ = pm.teardown();
    }

    let sandbox_arc = app.panels[idx].take_sandbox();
    let supervisor_name = app.panels[idx].supervisor_name().map(|s| s.to_string());
    let agent_name = app.panels[idx].agent_name.clone();

    app.panels.remove(idx);
    if app.panels.is_empty() {
        app.focused_panel = 0;
        app.focus_global();
        app.zoomed = false;
    } else {
        if app.focused_panel >= app.panels.len() {
            app.focused_panel = app.panels.len() - 1;
        }
        // Ensure focused panel is visible.
        if !app.panels[app.focused_panel].visible {
            if let Some(next) = app.next_visible_panel(app.focused_panel) {
                app.focused_panel = next;
            } else {
                app.focused_panel = 0;
                app.focus_global();
                app.zoomed = false;
            }
        }
    }

    Some((agent_name, sandbox_arc, supervisor_name))
}

/// Spawn a background task to destroy a sandbox.
/// If a supervised sandbox with this name is already running, ask the panel to
/// attach to its console instead of starting a duplicate VM.
fn try_attach_supervisor(
    sandbox_name: &str,
    panel_idx: usize,
    tx: &tokio::sync::mpsc::UnboundedSender<AppEvent>,
) -> bool {
    if sandbox_name.is_empty() {
        return false;
    }
    if crate::supervisor::client::SupervisorClient::new(sandbox_name).is_running() {
        let short_id = sandbox_name.chars().take(8).collect::<String>();
        let _ = tx.send(AppEvent::SupervisorReady {
            panel_idx,
            name: sandbox_name.to_string(),
            short_id,
            project_mount: None,
            exec_pty: None,
        });
        true
    } else {
        false
    }
}

fn supervisor_sandbox_dir(name: &str) -> std::path::PathBuf {
    crate::supervisor::client::SupervisorClient::new(name)
        .sandbox_dir()
        .to_path_buf()
}

/// Spawn a supervisor-managed sandbox for a TUI panel and attach its console.
///
/// Mirrors `nanosb run`'s next-mode path: plan the deploy, materialize mounts
/// and config files, spawn the detached supervisor, wait for it to report
/// `Running`, then emit `AppEvent::SupervisorReady` so the panel attaches.
async fn spawn_supervisor_panel(
    config: sandbox::AgentSandboxConfig,
    panel_idx: usize,
    tx: &tokio::sync::mpsc::UnboundedSender<AppEvent>,
) {
    use crate::deploy;

    let name = config.sandbox.name.clone();
    let sandbox_dir = supervisor_sandbox_dir(&name);

    let _ = tx.send(AppEvent::SandboxCreating {
        panel_idx,
        message: "Planning deploy...".into(),
    });

    let project_mount = if config.sandbox.project.is_some() {
        let mut rc_for_mount = config.sandbox.clone();
        match sandbox::sandbox::setup_project_mount(&mut rc_for_mount, None) {
            Ok(pm) => pm,
            Err(e) => {
                let _ = tx.send(AppEvent::SandboxFailed {
                    panel_idx,
                    error: format!("Failed to set up project mount: {}", e),
                });
                return;
            }
        }
    } else {
        None::<sandbox::ProjectMount>
    };

    let (plan, rc) = deploy::deploy_plan_for(&config, &sandbox_dir);
    if let Err(e) = deploy::materialize_plan(&sandbox_dir, &plan) {
        if let Some(mut pm) = project_mount {
            let _ = pm.teardown();
        }
        let _ = tx.send(AppEvent::SandboxFailed {
            panel_idx,
            error: format!("Failed to materialize deploy plan: {}", e),
        });
        return;
    }

    let mut rc = rc;

    // Interactive mode: the real TTY comes from the in-guest exec agent's PTY,
    // not the VM console (krun_add_console_port_tty is unusable here). Stage the
    // agent, bridge a host vsock socket, and make it PID 1 so the panel can run
    // the agent through `ExecClient` with a PTY.
    let mut interactive_exec: Option<(String, Vec<String>)> = None;
    let mut exec_agent_mount: Option<runtime::config::ExtraMount> = None;
    if config.interactive {
        // The program the panel launches over the PTY. An explicit
        // `sandbox.command` (e.g. a shell for tests) overrides the agent command.
        let (program, args) = match &config.sandbox.command {
            Some(c) => (c.clone(), config.sandbox.command_args.clone()),
            None => (
                plan.agent_command.binary.clone(),
                plan.agent_command.args.clone(),
            ),
        };
        let agent_dir = sandbox_dir.join("agent");
        match deploy::stage_exec_agent(&agent_dir) {
            Ok(guest_path) => {
                let exec_sock = sandbox_dir.join("exec.sock");
                let _ = std::fs::remove_file(&exec_sock);
                rc.vsock_socket = Some(exec_sock.to_string_lossy().to_string());
                rc.vsock_port = Some(deploy::EXEC_VSOCK_PORT);
                rc.command = Some(guest_path);
                rc.command_args = vec![deploy::EXEC_VSOCK_PORT.to_string()];
                exec_agent_mount = Some(runtime::config::ExtraMount {
                    tag: "agent".to_string(),
                    host_path: agent_dir.to_string_lossy().to_string(),
                    target: "/agent".to_string(),
                    readonly: false,
                });
                interactive_exec = Some((program, args));
            }
            Err(e) => {
                if let Some(mut pm) = project_mount {
                    let _ = pm.teardown();
                }
                let _ = tx.send(AppEvent::SandboxFailed {
                    panel_idx,
                    error: format!("Interactive mode needs the exec agent: {}", e),
                });
                return;
            }
        }
    }

    let config_json = match serde_json::to_string(&rc) {
        Ok(json) => json,
        Err(e) => {
            let _ = tx.send(AppEvent::SandboxFailed {
                panel_idx,
                error: format!("Failed to serialize config: {}", e),
            });
            return;
        }
    };
    let (config_json, boot_env_json) = match deploy::extract_boot_env(&config_json) {
        Ok(pair) => pair,
        Err(e) => {
            let _ = tx.send(AppEvent::SandboxFailed {
                panel_idx,
                error: format!("Failed to prepare boot env: {}", e),
            });
            return;
        }
    };
    let mut extra_mounts = deploy::extra_mounts_from_plan(&plan);
    if let Some(mount) = exec_agent_mount {
        // Mount the exec agent FIRST: the guest init aborts the whole extra-mount
        // loop on the first failure, and agent-state mounts (e.g.
        // /home/developer/.claude) fail on images that lack that home dir. The
        // agent mount target (/agent) always resolves, so it must not be skipped.
        extra_mounts.insert(0, mount);
    }
    let extra_mounts_json = match serde_json::to_string(&extra_mounts) {
        Ok(json) => json,
        Err(e) => {
            let _ = tx.send(AppEvent::SandboxFailed {
                panel_idx,
                error: format!("Failed to serialize mounts: {}", e),
            });
            return;
        }
    };

    let _ = tx.send(AppEvent::SandboxCreating {
        panel_idx,
        message: "Booting microVM...".into(),
    });

    let origin = deploy::Origin::cli(Vec::new(), Vec::new());
    if let Err(e) = deploy::spawn_supervisor(
        &name,
        &config_json,
        &extra_mounts_json,
        &boot_env_json,
        &origin.to_json(),
        rc.timeout_secs,
    ) {
        let _ = tx.send(AppEvent::SandboxFailed {
            panel_idx,
            error: format!("Failed to spawn supervisor: {}", e),
        });
        return;
    }

    let client = crate::supervisor::client::SupervisorClient::new(&name);
    if let Err(e) = deploy::wait_supervisor_running(&client, 60).await {
        let _ = tx.send(AppEvent::SandboxFailed {
            panel_idx,
            error: format!("Supervisor did not start: {}", e),
        });
        return;
    }

    let short_id = name.chars().take(8).collect::<String>();
    let exec_pty = interactive_exec
        .map(|(program, args)| (sandbox_dir.join("exec.sock"), program, args));
    let _ = tx.send(AppEvent::SupervisorReady {
        panel_idx,
        name,
        short_id,
        project_mount,
        exec_pty,
    });
}

fn spawn_sandbox_destroy(sandbox_arc: Arc<Mutex<Sandbox>>) {
    tokio::spawn(async move {
        match Arc::try_unwrap(sandbox_arc) {
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

fn spawn_supervisor_stop(name: String) {
    tokio::task::spawn_blocking(move || {
        let client = crate::supervisor::client::SupervisorClient::new(&name);
        let _ = client.stop(false);
    });
}

/// Handle a single key event.
pub(crate) async fn handle_key_event(app: &mut App, key: KeyEvent, tx: &mpsc::UnboundedSender<AppEvent>) {
    // Pending reconnect confirmation: intercept y/n before anything else.
    if app.pending_reconnect.is_some() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let panel_indices = app.pending_reconnect.take().unwrap();
                app.system_messages.clear();
                app.system_message_ticks = None;
                for &idx in &panel_indices {
                    // Trigger reconnect for each affected panel.
                    let saved_focus = app.focused_panel;
                    app.focused_panel = idx;
                    Box::pin(handle_command(app, Command::Reconnect, tx)).await;
                    app.focused_panel = saved_focus;
                }
                return;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.pending_reconnect = None;
                app.system_messages.clear();
                app.system_message_ticks = None;
                return;
            }
            _ => return, // Swallow other keys while prompt is active
        }
    }

    // Loading mode: swallow all keystrokes except navigation.
    if app.input_focus == InputFocus::Panel {
        if let Some(panel) = app.panels.get(app.focused_panel) {
            if panel.mode == PanelMode::Loading {
                match key.code {
                    KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        app.focus_prev();
                    }
                    KeyCode::BackTab => {
                        app.focus_prev();
                    }
                    KeyCode::Tab => {
                        app.focus_next();
                    }
                    KeyCode::Esc => {
                        app.focus_global();
                    }
                    KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.focus_global();
                    }
                    KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if !app.panels.is_empty() {
                            app.zoomed = !app.zoomed;
                        }
                    }
                    _ => {} // Swallow everything else
                }
                return;
            }
        }
    }

    // Headless mode: handle scroll + navigation keys only.
    if app.input_focus == InputFocus::Panel {
        if let Some(panel) = app.panels.get(app.focused_panel) {
            if panel.mode == PanelMode::Headless {
                match key.code {
                    KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        app.focus_prev();
                    }
                    KeyCode::BackTab => {
                        app.focus_prev();
                    }
                    KeyCode::Tab => {
                        app.focus_next();
                    }
                    KeyCode::Esc => {
                        app.focus_global();
                    }
                    KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.focus_global();
                    }
                    KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if !app.panels.is_empty() {
                            app.zoomed = !app.zoomed;
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if let Some(panel) = app.panels.get_mut(app.focused_panel) {
                            if let Some(ref mut hs) = panel.headless_state {
                                hs.scroll_offset = hs.scroll_offset.saturating_sub(1);
                                hs.auto_scroll = false;
                            }
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if let Some(panel) = app.panels.get_mut(app.focused_panel) {
                            if let Some(ref mut hs) = panel.headless_state {
                                hs.scroll_offset = hs.scroll_offset.saturating_add(1);
                                hs.auto_scroll = false;
                            }
                        }
                    }
                    KeyCode::PageUp => {
                        if let Some(panel) = app.panels.get_mut(app.focused_panel) {
                            if let Some(ref mut hs) = panel.headless_state {
                                hs.scroll_offset = hs.scroll_offset.saturating_sub(20);
                                hs.auto_scroll = false;
                            }
                        }
                    }
                    KeyCode::PageDown => {
                        if let Some(panel) = app.panels.get_mut(app.focused_panel) {
                            if let Some(ref mut hs) = panel.headless_state {
                                hs.scroll_offset = hs.scroll_offset.saturating_add(20);
                                hs.auto_scroll = false;
                            }
                        }
                    }
                    KeyCode::Home => {
                        if let Some(panel) = app.panels.get_mut(app.focused_panel) {
                            if let Some(ref mut hs) = panel.headless_state {
                                hs.scroll_offset = 0;
                                hs.auto_scroll = false;
                            }
                        }
                    }
                    KeyCode::End => {
                        if let Some(panel) = app.panels.get_mut(app.focused_panel) {
                            if let Some(ref mut hs) = panel.headless_state {
                                hs.auto_scroll = true;
                            }
                        }
                    }
                    _ => {} // Swallow other keys
                }
                return;
            }
        }
    }

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
                    // Ctrl+G: escape to global bar (Esc is forwarded to terminal)
                    KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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
                    // Ctrl+V: check clipboard for image, upload if found.
                    // If no image, forward the keystroke to the terminal.
                    // Note: Cmd+V on macOS is handled by the terminal emulator
                    // and arrives as CrosstermEvent::Paste, handled separately.
                    KeyCode::Char(ch)
                        if ((ch == 'v' || ch == 'V')
                            && (key.modifiers.contains(KeyModifiers::CONTROL)
                                || key.modifiers.contains(KeyModifiers::SUPER)))
                            // Some Windows terminals emit Ctrl+V as SYN (0x16)
                            // without reporting CONTROL in modifiers.
                            => {
                        let focused_panel = app.focused_panel;
                        let panel_mode = app
                            .panels
                            .get(focused_panel)
                            .map(|p| p.mode)
                            .unwrap_or(PanelMode::Loading);
                        tracing::info!(
                            panel_idx = focused_panel,
                            mode = ?panel_mode,
                            key_char = ?ch,
                            modifiers = ?key.modifiers,
                            "Paste key detected (Ctrl/Cmd+V)"
                        );

                        // Clone the write channel so the async task can still
                        // paste text even if SSH metadata is not ready yet.
                        let write_tx = app.panels.get(app.focused_panel)
                            .and_then(|p| p.terminal_handle.as_ref())
                            .map(|h| h.write_tx.clone());
                        let upload_info = panel_mount_root(app);
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            tracing::debug!(
                                has_upload_info = upload_info.is_some(),
                                has_write_tx = write_tx.is_some(),
                                "Handling Ctrl/Cmd+V paste"
                            );

                            // Paste text immediately.
                            let text_result = tokio::task::spawn_blocking(
                                super::upload::read_clipboard_text,
                            )
                            .await;

                            let text = match text_result {
                                Ok(Ok(text)) => text,
                                Ok(Err(e)) => {
                                    tracing::debug!(error = %e, "Clipboard text read failed");
                                    String::new()
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "Clipboard text task failed");
                                    String::new()
                                }
                            };

                            if !text.is_empty() {
                                if let Some(ref wtx) = write_tx {
                                    let sent = send_bracketed_paste(wtx, &text);
                                    tracing::debug!(
                                        text_len = text.len(),
                                        sent,
                                        "Forwarded clipboard text as bracketed paste"
                                    );
                                } else {
                                    tracing::warn!(
                                        text_len = text.len(),
                                        "Clipboard text available but terminal write channel missing"
                                    );
                                }
                                return;
                            }

                            if let Some((panel_idx, mount_root)) = upload_info {
                                if mount_root.is_none() {
                                    tracing::debug!("Clipboard paste produced empty text and no mount");
                                    return;
                                }
                                let image = tokio::task::spawn_blocking(
                                    super::upload::read_clipboard_image,
                                )
                                .await;
                                if let Ok(Ok((png_bytes, filename))) = image {
                                    tracing::debug!(
                                        panel_idx,
                                        filename = %filename,
                                        bytes = png_bytes.len(),
                                        "Clipboard image detected; starting upload"
                                    );
                                    super::upload::spawn_bytes_upload(
                                        mount_root, png_bytes, filename,
                                        panel_idx, tx,
                                    );
                                    return;
                                }
                            }
                            tracing::debug!("Clipboard paste produced empty text and no image");
                        });
                        return;
                    }
                    // Scrollback: Shift+PageUp/PageDown to scroll terminal history.
                    KeyCode::PageUp if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        if let Some(panel) = app.panels.get_mut(app.focused_panel) {
                            if let Some(ref mut term) = panel.terminal {
                                let half_page = (panel.last_terminal_size.1 as usize / 2).max(1);
                                term.scroll_up(half_page);
                            }
                        }
                        return;
                    }
                    KeyCode::PageDown if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        if let Some(panel) = app.panels.get_mut(app.focused_panel) {
                            if let Some(ref mut term) = panel.terminal {
                                let half_page = (panel.last_terminal_size.1 as usize / 2).max(1);
                                term.scroll_down(half_page);
                            }
                        }
                        return;
                    }
                    KeyCode::Home if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        if let Some(panel) = app.panels.get_mut(app.focused_panel) {
                            if let Some(ref mut term) = panel.terminal {
                                term.scroll_up(usize::MAX);
                            }
                        }
                        return;
                    }
                    KeyCode::End if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        if let Some(panel) = app.panels.get_mut(app.focused_panel) {
                            if let Some(ref mut term) = panel.terminal {
                                term.scroll_to_bottom();
                            }
                        }
                        return;
                    }
                    _ => {
                        // Any forwarded keystroke snaps back to live view.
                        if let Some(panel) = app.panels.get_mut(app.focused_panel) {
                            if let Some(ref mut term) = panel.terminal {
                                if term.is_scrolled_up() {
                                    term.scroll_to_bottom();
                                }
                            }
                        }
                        // Forward everything else to SSH terminal.
                        let bytes = super::terminal::crossterm_key_to_bytes(key);
                        if !bytes.is_empty() {
                            if let Some(panel) = app.panels.get_mut(app.focused_panel) {
                                panel.had_interaction = true;
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
                        app.sidebar_committed_scroll =
                            app.sidebar_committed_scroll.saturating_sub(1);
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

        // Esc: dismiss popup → dismiss autocomplete → return to global bar → close sidebar.
        KeyCode::Esc => {
            if !app.system_messages.is_empty() && !app.panels.is_empty() {
                app.system_messages.clear();
                app.system_message_ticks = None;
            } else if app.autocomplete_index.is_some() {
                app.autocomplete_index = None;
            } else if app.input_focus == InputFocus::Panel {
                app.focus_global();
            } else {
                app.show_mcp_sidebar = false;
            }
        }

        // Ctrl+C: clear the global input bar.
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if app.input_focus == InputFocus::Global {
                app.global_input.clear();
                app.autocomplete_index = None;
                app.command_history.reset_navigation();
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

            // Capture raw input before handle_submit() clears the buffer.
            let raw_input = app.current_input().to_string();

            let result = app.handle_submit();
            match result {
                SubmitResult::Command(cmd) => {
                    // Record in history (skip /clearhistory itself).
                    if !matches!(cmd, Command::ClearHistory) {
                        app.command_history.push(&raw_input);
                    }
                    handle_command(app, cmd, tx).await;
                }
                SubmitResult::CommandError(msg) => {
                    app.set_system_message_timed(
                        ChatMessage {
                            role: MessageRole::System,
                            content: msg,
                        },
                        SYSTEM_POPUP_TICKS_DEFAULT,
                    );
                }
                SubmitResult::Message(_msg) => {
                    // Panels are terminal-only; messages are not sent.
                }
                SubmitResult::Empty | SubmitResult::NoPanel => {
                    // Nothing to do.
                }
            }
            app.command_history.reset_navigation();
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

        // Up/Down: autocomplete → multiline input navigation → history → chat scroll.
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
                } else if app.input_focus == InputFocus::Global {
                    let current_text = app.global_input.text().to_string();
                    if let Some(entry) = app.command_history.navigate_up(&current_text) {
                        let entry = entry.to_string();
                        app.global_input.set_text(entry);
                    }
                }
            }
        }
        KeyCode::Down => {
            if app.autocomplete_active() {
                let suggestions = commands::autocomplete(app.current_input());
                if !suggestions.is_empty() {
                    let current = app
                        .autocomplete_index
                        .unwrap_or(suggestions.len().saturating_sub(1));
                    app.autocomplete_index = Some((current + 1) % suggestions.len());
                }
            } else {
                let width = app.active_input_width() as usize;
                let total_lines = app.active_input().visual_line_count(width);
                let (row, _) = app.active_input().cursor_visual_position(width);
                if row + 1 < total_lines {
                    app.handle_move_down(width);
                } else if app.input_focus == InputFocus::Global
                    && app.command_history.is_navigating()
                {
                    match app.command_history.navigate_down() {
                        Some(entry) => {
                            let entry = entry.to_string();
                            app.global_input.set_text(entry);
                        }
                        None => {
                            let draft = app.command_history.draft().to_string();
                            app.global_input.set_text(draft);
                        }
                    }
                }
            }
        }

        _ => {}
    }
}

/// Handle a parsed slash command.
pub(crate) async fn handle_command(app: &mut App, cmd: Command, tx: &mpsc::UnboundedSender<AppEvent>) {
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
            app.set_system_message_persistent(ChatMessage {
                role: MessageRole::System,
                content: commands::format_help(),
            });
        }
        Command::Close { target } => {
            let idx = match app.resolve_panel_target(target.as_deref()) {
                Some(i) => i,
                None => {
                    let msg = match target {
                        Some(t) => format!("No panel matching '{}'.", t),
                        None => "No panels to close.".to_string(),
                    };
                    app.set_system_message(ChatMessage {
                        role: MessageRole::System,
                        content: msg,
                    });
                    return;
                }
            };

            if !app.panels[idx].visible {
                app.set_status_message("Panel is already hidden.");
                return;
            }

            app.panels[idx].visible = false;
            let name = app.panels[idx]
                .display_name
                .as_deref()
                .unwrap_or(&app.panels[idx].agent_name)
                .to_string();
            app.set_status_message(format!("Hidden '{}'. Use /open to show.", name));

            if app.focused_panel == idx {
                if let Some(next) = app.next_visible_panel(idx) {
                    app.focused_panel = next;
                } else {
                    app.focused_panel = 0;
                    app.focus_global();
                    app.zoomed = false;
                }
            }
        }
        Command::Open { target } => {
            let idx = match target.as_deref() {
                None => {
                    // No arg: find the most recently hidden panel (highest index).
                    app.panels
                        .iter()
                        .enumerate()
                        .rev()
                        .find(|(_, p)| !p.visible)
                        .map(|(i, _)| i)
                }
                Some(s) => app.resolve_panel_target(Some(s)),
            };

            match idx {
                Some(i) if i < app.panels.len() => {
                    if app.panels[i].visible {
                        app.set_status_message("Panel is already visible.");
                    } else {
                        app.panels[i].visible = true;
                        let name = app.panels[i]
                            .display_name
                            .as_deref()
                            .unwrap_or(&app.panels[i].agent_name)
                            .to_string();
                        app.set_status_message(format!("Showing '{}'.", name));
                        app.focused_panel = i;
                        app.focus_panel_input();
                    }
                }
                _ => {
                    let msg = match target {
                        Some(t) => format!("No hidden panel matching '{}'.", t),
                        None => "No hidden panels.".to_string(),
                    };
                    app.set_status_message(msg);
                }
            }
        }
        Command::Focus { panel } => {
            if panel < app.panels.len() {
                if !app.panels[panel].visible {
                    app.set_status_message("Panel is hidden. Use /open to show it first.");
                } else {
                    app.focused_panel = panel;
                    app.focus_panel_input();
                }
            } else {
                app.set_system_message(ChatMessage {
                    role: MessageRole::System,
                    content: format!("No panel {}. Use /add <agent> first.", panel),
                });
            }
        }
        Command::McpToggle => {
            app.show_mcp_sidebar = !app.show_mcp_sidebar;
        }
        Command::AddAgent {
            agent,
            image,
            tag,
            project,
            branch,
            name,
            auto_mode,
            interactive,
            prompt,
            model,
            use_env,
            env_file,
            run_as_root,
        } => {
            add_agent(
                app,
                &agent,
                image.as_deref(),
                tag.as_deref(),
                project.as_deref(),
                branch.as_deref(),
                name.as_deref(),
                auto_mode,
                interactive,
                prompt.as_deref(),
                model.as_deref(),
                &use_env,
                env_file.as_deref(),
                run_as_root,
                tx,
            );
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
            // Pre-calculate panel dimensions before mutable borrow.
            let (pty_cols, pty_rows) = {
                let term_size = ratatui::crossterm::terminal::size().unwrap_or((160, 40));
                let has_sidebar = app.show_mcp_sidebar || app.show_sandbox_sidebar;
                super::grid::estimate_panel_inner_size(
                    term_size.0,
                    term_size.1,
                    app.visible_panel_count(),
                    has_sidebar,
                    app.zoomed,
                )
            };
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                panel.terminal = None;
                panel.terminal_handle = None;
                panel.opened_urls.clear();
                panel.pending_urls.clear();
                panel.reconnecting = true;
                panel.mode = PanelMode::Loading;
                panel.loading_error = None;
                panel.loading_tick = 0;

                let console_name = panel.supervisor_name().map(|s| s.to_string());
                let exec_pty = panel.exec_pty.clone();

                if let Some(name) = console_name {
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let result = match exec_pty {
                            Some((sock, program, args)) => {
                                super::terminal::connect_exec_pty(
                                    sock, program, args, pty_cols, pty_rows, panel_idx,
                                    tx.clone(),
                                )
                                .await
                            }
                            None => {
                                super::terminal::connect_console(
                                    name, pty_cols, pty_rows, panel_idx, tx.clone(),
                                )
                                .await
                            }
                        };
                        match result {
                            Ok(handle) => {
                                let _ = tx.send(AppEvent::SshConnected { panel_idx, handle });
                            }
                            Err(e) => {
                                let _ = tx.send(AppEvent::SshDisconnected {
                                    panel_idx,
                                    error: Some(format!("Console reconnect failed: {}", e)),
                                });
                            }
                        }
                    });
                } else {
                    panel.loading_error =
                        Some("No supervised sandbox for this panel. Cannot reconnect.".to_string());
                    panel.reconnecting = false;
                }
            } else {
                app.set_system_message(ChatMessage {
                    role: MessageRole::System,
                    content: "No panel focused. Use /add <agent> first.".to_string(),
                });
            }
        }
        Command::Kill { panel } => {
            let idx = match app.resolve_panel_target(panel.as_deref()) {
                Some(i) => i,
                None => {
                    let msg = match panel {
                        Some(t) => format!("No panel matching '{}'.", t),
                        None => "No panels to kill.".to_string(),
                    };
                    app.set_system_message(ChatMessage {
                        role: MessageRole::System,
                        content: msg,
                    });
                    return;
                }
            };

            if let Some((agent_name, sandbox_arc, supervisor_name)) = kill_panel_at(app, idx) {
                if let Some(sb) = sandbox_arc {
                    spawn_sandbox_destroy(sb);
                }
                if let Some(name) = supervisor_name {
                    spawn_supervisor_stop(name);
                }

                app.set_system_message(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Killed '{}'.", agent_name),
                });
            }
        }
        Command::McpList => {
            if app.panels.is_empty() {
                app.set_system_message(ChatMessage {
                    role: MessageRole::System,
                    content: "MCP commands require an active panel. Use /add <agent> first."
                        .to_string(),
                });
            } else {
                handle_mcp_list(app).await;
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
                let dir = dir.clone();
                let msg = tokio::task::spawn_blocking(move || {
                    let output = super::gitcmd::host_git()
                        .args(["branch", "--list", "nanosb/*"])
                        .current_dir(&dir)
                        .output();
                    match output {
                        Ok(out) => {
                            let branches = String::from_utf8_lossy(&out.stdout);
                            if branches.trim().is_empty() {
                                "No nanosb branches found.".to_string()
                            } else {
                                format!("Nanosb branches:\n{}", branches)
                            }
                        }
                        Err(e) => format!("Failed to list branches: {}", e),
                    }
                })
                .await
                .unwrap_or_else(|e| format!("Task failed: {}", e));
                let chat_msg = ChatMessage {
                    role: MessageRole::System,
                    content: msg,
                };
                app.set_system_message(chat_msg);
            } else {
                app.set_system_message(ChatMessage {
                    role: MessageRole::System,
                    content: "No project configured. Use --project flag when launching nanosb."
                        .to_string(),
                });
            }
        }
        Command::Sync { dry_run: _, action } => {
            let panel_idx = app.focused_panel;
            match action.as_deref() {
                None => {
                    // Show sync status
                    let auto = app
                        .panels
                        .get(panel_idx)
                        .and_then(|p| p.sync_override)
                        .unwrap_or(app.settings.gitsync.auto_sync);
                    let status_label = if auto { "ON (unsafe)" } else { "OFF (safe)" };
                    let has_branch = app
                        .panels
                        .get(panel_idx)
                        .and_then(|p| p.project_mount.as_ref())
                        .map(|pm| !pm.created_branches.is_empty())
                        .unwrap_or(false);
                    let branch_info = if has_branch {
                        app.panels
                            .get(panel_idx)
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
                        if app.settings.gitsync.notify_on_commit {
                            "ON"
                        } else {
                            "OFF"
                        },
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
                            // Fetch current state to namespaced ref (no --force needed).
                            if let Some(ref wt_base) = pm.worktree_base {
                                if let Some((source, branch)) = pm.created_branches.first() {
                                    let short_id = branch.trim_start_matches("refs/heads/").trim_start_matches("nanosb/");
                                    let nanosb_ref = format!("refs/nanosb/{}", short_id);
                                    let refspec = format!("+{}:{}", branch, nanosb_ref);
                                    let wt = wt_base.clone();
                                    let src = source.clone();
                                    let ok = tokio::task::spawn_blocking(move || {
                                        super::gitcmd::host_git()
                                            .args([
                                                "fetch",
                                                &wt.to_string_lossy(),
                                                &refspec,
                                            ])
                                            .current_dir(&src)
                                            .output()
                                            .map(|o| o.status.success())
                                            .unwrap_or(false)
                                    })
                                    .await
                                    .unwrap_or(false);
                                    let msg = if ok {
                                        format!("Synced to '{}'.", nanosb_ref)
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
        Command::Edit { tool } => {
            let panel_idx = app.focused_panel;
            let clone_path = app
                .panels
                .get(panel_idx)
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
            let editor_pref = tool.as_deref().unwrap_or(&app.settings.tools.editor);

            // Handle custom command template
            if let Some(ref cmd_template) = app.settings.tools.custom_command {
                if editor_pref == "custom"
                    || (editor_pref == "auto" && sandbox::settings::resolve_tool("auto").is_none())
                {
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

            let resolved = sandbox::settings::resolve_tool(editor_pref);

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
                    if let Some(app_name) = sandbox::settings::macos_app_name(editor_pref) {
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
        Command::Theme { name } => {
            use crate::tui::theme::{Theme, ThemeName, ALL_THEME_NAMES};
            match name {
                None => {
                    // List available themes with current highlighted.
                    let current = app.theme_name.to_string();
                    let list: Vec<String> = ALL_THEME_NAMES
                        .iter()
                        .map(|n| {
                            if *n == current {
                                format!("  * {} (active)", n)
                            } else {
                                format!("    {}", n)
                            }
                        })
                        .collect();
                    let msg = format!("Available themes:\n{}", list.join("\n"));
                    app.set_system_message(ChatMessage {
                        role: MessageRole::System,
                        content: msg,
                    });
                }
                Some(name_str) => {
                    // parse_theme already validated the name, but be safe.
                    match name_str.parse::<ThemeName>() {
                        Ok(tn) => {
                            app.theme = Theme::by_name(tn);
                            app.theme_name = tn;
                            app.settings.ui.theme = tn.to_string();
                            let _ = app.settings.save();
                            app.set_status_message(format!("Theme set to '{}'.", tn));
                        }
                        Err(msg) => {
                            app.set_status_message(msg);
                        }
                    }
                }
            }
        }

        // ===== Skills commands =====
        Command::SkillsList | Command::SkillsShow { .. } => {
            if app.panels.is_empty() {
                app.set_system_message(ChatMessage {
                    role: MessageRole::System,
                    content: "Skills commands require an active panel. Use /add <agent> first."
                        .to_string(),
                });
            } else {
                match cmd {
                    Command::SkillsList => handle_skills_list(app).await,
                    Command::SkillsShow { name } => handle_skills_show(app, &name),
                    _ => unreachable!(),
                }
            }
        }

        // ===== Agent commands =====
        Command::AgentShow => {
            if app.panels.is_empty() {
                app.set_system_message(ChatMessage {
                    role: MessageRole::System,
                    content: "Agent commands require an active panel. Use /add <agent> first."
                        .to_string(),
                });
            } else {
                handle_agent_show(app).await;
            }
        }
        Command::AgentList => handle_agent_list(app),
        Command::AgentInfo { name } => handle_agent_info(app, &name),
        Command::Upload { path } => {
            handle_upload(app, &path, tx);
        }
        Command::PasteImage => {
            handle_paste_image(app, tx);
        }
        Command::Destroy => {
            // Full cleanup: teardown all projects, delete session, exit.
            // Mark for destroy so the shutdown path knows to do full teardown.
            app.destroy_on_quit = true;
            app.should_quit = true;
        }
        Command::ClearHistory => {
            app.command_history.clear();
            app.set_status_message("Command history cleared.");
        }
        Command::Projects { forget } => {
            let registry = sandbox::ProjectRegistry::load();
            match forget {
                None => {
                    // List projects.
                    let projects = registry.list();
                    if projects.is_empty() {
                        app.set_system_message(ChatMessage {
                            role: MessageRole::System,
                            content: "No projects registered. Launch nanosb with --project to register one.".to_string(),
                        });
                    } else {
                        let lines: Vec<String> = projects
                            .iter()
                            .enumerate()
                            .map(|(i, entry)| {
                                let age = {
                                    let now = chrono::Utc::now();
                                    let duration = now.signed_duration_since(entry.last_used);
                                    let secs = duration.num_seconds();
                                    if secs < 60 {
                                        format!("{}s ago", secs.max(0))
                                    } else if secs < 3600 {
                                        format!("{}m ago", duration.num_minutes())
                                    } else if secs < 86_400 {
                                        format!("{}h ago", duration.num_hours())
                                    } else {
                                        format!("{}d ago", duration.num_days())
                                    }
                                };
                                format!(
                                    "{}. {} ({}) — last used {}",
                                    i + 1,
                                    entry.display_name,
                                    entry.path,
                                    age,
                                )
                            })
                            .collect();
                        let msg = format!("Registered projects:\n{}", lines.join("\n"));
                        app.set_system_message(ChatMessage {
                            role: MessageRole::System,
                            content: msg,
                        });
                    }
                }
                Some(path) => {
                    let mut registry = registry;
                    let path = std::path::Path::new(&path);
                    if registry.forget(path) {
                        app.set_system_message(ChatMessage {
                            role: MessageRole::System,
                            content: format!("Forgot project '{}'.", path.display()),
                        });
                    } else {
                        app.set_system_message(ChatMessage {
                            role: MessageRole::System,
                            content: format!("Project '{}' was not in the registry.", path.display()),
                        });
                    }
                }
            }
        }
        Command::Diff { .. }
        | Command::Status
        | Command::Discard
        | Command::Mounts
        | Command::Exec { .. }
        | Command::Logs { .. }
        | Command::Stop { .. } => {
            app.set_status_message("Not yet implemented.");
        }
    }
}

/// Copy focused panel content to the system clipboard.
fn handle_copy(app: &mut App) {
    let panel = match app.focused_panel_mut() {
        Some(p) => p,
        None => {
            app.set_system_message(ChatMessage {
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
/// Copy text to system clipboard without blocking the async event loop.
///
/// Uses `arboard` (cross-platform Rust clipboard library) instead of
/// spawning external processes like `pbcopy` / `xclip`.
fn copy_to_clipboard(text: &str) -> std::result::Result<(), String> {
    arboard::Clipboard::new()
        .map_err(|e| format!("clipboard: {}", e))?
        .set_text(text.to_string())
        .map_err(|e| format!("clipboard set: {}", e))
}

/// Get the SSH port and key path from the focused panel, if available.
fn panel_mount_root(app: &App) -> Option<(usize, Option<std::path::PathBuf>)> {
    let idx = app.focused_panel;
    let panel = app.panels.get(idx)?;
    let mount_root = panel
        .project_mount
        .as_ref()
        .and_then(|pm| pm.worktree_base.clone());
    Some((idx, mount_root))
}

/// Resolve a user-supplied path: strip quotes, expand `~`, resolve relative paths.
fn resolve_upload_path(raw: &str) -> std::path::PathBuf {
    // Strip surrounding quotes.
    let trimmed = raw.trim().trim_matches('\'').trim_matches('"');

    // Expand leading ~ to home directory.
    let expanded = if trimmed == "~" {
        dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("~"))
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        match dirs::home_dir() {
            Some(home) => home.join(rest),
            None => std::path::PathBuf::from(trimmed),
        }
    } else {
        std::path::PathBuf::from(trimmed)
    };

    // Resolve relative paths against the current working directory.
    if expanded.is_relative() {
        std::env::current_dir()
            .map(|cwd| cwd.join(&expanded))
            .unwrap_or(expanded)
    } else {
        expanded
    }
}

/// Handle the `/upload <path>` command.
fn handle_upload(app: &mut App, path: &str, tx: &mpsc::UnboundedSender<AppEvent>) {
    let (panel_idx, mount_root) = match panel_mount_root(app) {
        Some(info) => info,
        None => {
            app.set_system_message(ChatMessage {
                role: MessageRole::System,
                content: "No panel to upload to. Use /add <agent> first.".to_string(),
            });
            return;
        }
    };
    if mount_root.is_none() {
        app.set_system_message(ChatMessage {
            role: MessageRole::System,
            content: "No project mount for this panel. Uploads require a mounted workspace."
                .to_string(),
        });
        return;
    }

    let host_path = resolve_upload_path(path);
    if !host_path.exists() {
        app.set_system_message(ChatMessage {
            role: MessageRole::System,
            content: format!("File not found: {}", host_path.display()),
        });
        return;
    }
    if !host_path.is_file() {
        app.set_system_message(ChatMessage {
            role: MessageRole::System,
            content: format!("Not a file: {}", host_path.display()),
        });
        return;
    }

    let filename = host_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    app.set_status_message(format!("Uploading {}...", filename));

    super::upload::spawn_file_upload(mount_root, host_path, panel_idx, tx.clone());
}

/// Handle the `/paste-image` command.
fn handle_paste_image(app: &mut App, tx: &mpsc::UnboundedSender<AppEvent>) {
    let (panel_idx, mount_root) = match panel_mount_root(app) {
        Some(info) => info,
        None => {
            app.set_system_message(ChatMessage {
                role: MessageRole::System,
                content: "No panel to upload to. Use /add <agent> first.".to_string(),
            });
            return;
        }
    };
    if mount_root.is_none() {
        app.set_system_message(ChatMessage {
            role: MessageRole::System,
            content: "No project mount for this panel. Uploads require a mounted workspace."
                .to_string(),
        });
        return;
    }

    app.set_status_message("Reading clipboard image...");
    let tx = tx.clone();

    tokio::spawn(async move {
        // Clipboard access is blocking — run in spawn_blocking.
        let result = tokio::task::spawn_blocking(super::upload::read_clipboard_image).await;

        match result {
            Ok(Ok((png_bytes, filename))) => {
                super::upload::spawn_bytes_upload(mount_root, png_bytes, filename, panel_idx, tx);
            }
            Ok(Err(e)) => {
                let _ = tx.send(AppEvent::UploadFailed {
                    panel_idx,
                    error: e,
                });
            }
            Err(e) => {
                let _ = tx.send(AppEvent::UploadFailed {
                    panel_idx,
                    error: format!("Clipboard task panicked: {}", e),
                });
            }
        }
    });
}

/// Handle a bracketed paste event.
///
/// On macOS, Cmd+V is intercepted by the terminal emulator and arrives here
/// as a `CrosstermEvent::Paste`. When the clipboard contains only an image
/// (no text), the terminal sends an empty paste — we detect that and check
/// the clipboard for an image to upload.
fn handle_paste_event(app: &mut App, text: String, tx: &mpsc::UnboundedSender<AppEvent>) {
    tracing::info!(
        text_len = text.len(),
        focused_panel = app.focused_panel,
        input_focus = ?app.input_focus,
        "Processing paste event"
    );

    // If focus is on the global input bar, insert the text there.
    if app.input_focus == InputFocus::Global {
        app.global_input.insert_str(&text);
        tracing::debug!(text_len = text.len(), "Inserted paste into global input");
        return;
    }

    // In Terminal mode: if the paste is empty (image-only clipboard via Cmd+V),
    // check the clipboard for an image to upload.
    if text.is_empty() {
        if let Some((panel_idx, mount_root)) = panel_mount_root(app) {
            if mount_root.is_none() {
                tracing::debug!("Empty paste ignored: no project mount for panel");
                return;
            }
            let tx = tx.clone();
            tokio::spawn(async move {
                tracing::debug!(panel_idx, "Empty paste; checking clipboard image");
                let result = tokio::task::spawn_blocking(super::upload::read_clipboard_image).await;
                match result {
                    Ok(Ok((png_bytes, filename))) => {
                        tracing::debug!(
                            panel_idx,
                            filename = %filename,
                            bytes = png_bytes.len(),
                            "Empty paste resolved to clipboard image upload"
                        );
                        super::upload::spawn_bytes_upload(
                            mount_root, png_bytes, filename, panel_idx, tx,
                        );
                    }
                    Ok(Err(e)) => {
                        tracing::debug!(panel_idx, error = %e, "No clipboard image for empty paste");
                        let _ = tx.send(AppEvent::UploadFailed {
                            panel_idx,
                            error: e,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(panel_idx, error = %e, "Clipboard image task failed for empty paste");
                        let _ = tx.send(AppEvent::UploadFailed {
                            panel_idx,
                            error: format!("Clipboard task panicked: {}", e),
                        });
                    }
                }
            });
        } else {
            tracing::debug!("Empty paste ignored: no panel info available");
        }
        return;
    }

    // Forward pasted text to the terminal wrapped in bracketed-paste escape
    // sequences (\x1b[200~ ... \x1b[201~).  Shells that support bracketed
    // paste (bash 4.4+, zsh, fish) buffer the entire paste and process it
    // in one shot instead of interpreting each character individually, which
    // dramatically speeds up large pastes.
    if let Some(panel) = app.panels.get(app.focused_panel) {
        if panel.mode == PanelMode::Terminal {
            if let Some(ref handle) = panel.terminal_handle {
                let sent = send_bracketed_paste(&handle.write_tx, &text);
                tracing::debug!(
                    panel_idx = app.focused_panel,
                    text_len = text.len(),
                    sent,
                    "Forwarded Crossterm paste to terminal"
                );
            }
        }
    }
}

fn send_bracketed_paste(write_tx: &mpsc::UnboundedSender<Vec<u8>>, text: &str) -> bool {
    let mut buf = Vec::with_capacity(text.len() + 12);
    buf.extend_from_slice(b"\x1b[200~");
    buf.extend_from_slice(text.as_bytes());
    buf.extend_from_slice(b"\x1b[201~");
    write_tx.send(buf).is_ok()
}

/// Handle a mouse event for panel-scoped text selection.
pub(crate) fn handle_mouse_event(app: &mut App, mouse: MouseEvent) {
    let x = mouse.column;
    let y = mouse.row;

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Clear any existing selection.
            app.mouse_selection = None;

            // Find which panel inner area contains (x, y).
            if let Some((panel_idx, inner_area)) = find_panel_at(app, x, y) {
                // Convert absolute coords to panel-relative terminal coords.
                let term_col = x.saturating_sub(inner_area.x);
                let term_row = y.saturating_sub(inner_area.y);

                // Only start selection in terminal mode panels.
                if app
                    .panels
                    .get(panel_idx)
                    .is_some_and(|p| p.mode == PanelMode::Terminal && p.terminal.is_some())
                {
                    app.mouse_selection = Some(MouseSelection {
                        panel_idx,
                        start: (term_row, term_col),
                        end: (term_row, term_col),
                        dragging: true,
                    });
                }

                // Focus the clicked panel.
                if panel_idx != app.focused_panel {
                    app.focused_panel = panel_idx;
                    app.input_focus = InputFocus::Panel;
                }
            }
        }

        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(ref mut sel) = app.mouse_selection {
                if sel.dragging {
                    // Find the inner area for the selection's panel.
                    let inner_area = app
                        .panel_areas
                        .iter()
                        .find(|(idx, _)| *idx == sel.panel_idx)
                        .map(|(_, area)| *area);

                    if let Some(inner_area) = inner_area {
                        // Clamp to panel boundaries and convert to term coords.
                        let clamped_x = x.clamp(
                            inner_area.x,
                            inner_area.x + inner_area.width.saturating_sub(1),
                        );
                        let clamped_y = y.clamp(
                            inner_area.y,
                            inner_area.y + inner_area.height.saturating_sub(1),
                        );
                        let term_col = clamped_x.saturating_sub(inner_area.x);
                        let term_row = clamped_y.saturating_sub(inner_area.y);
                        sel.end = (term_row, term_col);
                    }
                }
            }
        }

        MouseEventKind::Up(MouseButton::Left) => {
            if let Some(ref mut sel) = app.mouse_selection {
                sel.dragging = false;

                // If start == end, this was a click (no drag) — just focus, don't copy.
                if sel.start == sel.end {
                    app.mouse_selection = None;
                    return;
                }

                // Extract text from the vt100 screen and copy to clipboard.
                let (start, end) = sel.normalized();
                if let Some(panel) = app.panels.get(sel.panel_idx) {
                    if let Some(ref term) = panel.terminal {
                        let text = term
                            .screen()
                            .contents_between(start.0, start.1, end.0, end.1);
                        if !text.is_empty() {
                            let _ = copy_to_clipboard(&text);
                            app.set_status_message(format!(
                                "Copied {} chars to clipboard.",
                                text.len()
                            ));
                        }
                    }
                }
            }
        }

        MouseEventKind::ScrollUp => {
            if let Some((panel_idx, _)) = find_panel_at(app, x, y) {
                if let Some(panel) = app.panels.get_mut(panel_idx) {
                    if panel.mode == PanelMode::Terminal {
                        if let Some(ref mut term) = panel.terminal {
                            term.scroll_up(3);
                        }
                    }
                }
            }
        }
        MouseEventKind::ScrollDown => {
            if let Some((panel_idx, _)) = find_panel_at(app, x, y) {
                if let Some(panel) = app.panels.get_mut(panel_idx) {
                    if panel.mode == PanelMode::Terminal {
                        if let Some(ref mut term) = panel.terminal {
                            term.scroll_down(3);
                        }
                    }
                }
            }
        }

        _ => {}
    }
}

/// Find which panel's inner area contains the given absolute coordinates.
fn find_panel_at(app: &App, x: u16, y: u16) -> Option<(usize, ratatui::layout::Rect)> {
    for &(panel_idx, area) in &app.panel_areas {
        if x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height {
            return Some((panel_idx, area));
        }
    }
    None
}

/// Known agent type names for CLI command resolution and API key detection.
const KNOWN_AGENTS: &[&str] = &["claude", "codex", "goose", "cursor"];

/// Detect the base agent type from a Docker image name.
///
/// Extracts a known agent name from image patterns like:
/// - `localhost:5050/agent-claude:latest` → `"claude"`
/// - `ghcr.io/nanosandboxai/agents-registry/codex:v1` → `"codex"`
/// - `nanosb-goose:latest` → `"goose"`
/// Parse raw SSH bytes as NDJSON and update the headless state.
///
/// Each agent emits NDJSON events in a slightly different schema, but the
/// general pattern is the same: text deltas, tool calls, tool results, and
/// a final result message. This parser is best-effort — non-JSON lines
/// (shell prompts, ANSI junk) are silently ignored.
fn parse_headless_data(state: &mut super::app::HeadlessState, data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    state.line_buffer.push_str(&text);

    while let Some(newline_pos) = state.line_buffer.find('\n') {
        let line = state.line_buffer[..newline_pos].trim().to_string();
        state.line_buffer = state.line_buffer[newline_pos + 1..].to_string();

        if line.is_empty() {
            continue;
        }

        // Strip any stray ANSI escape sequences (belt-and-suspenders for non-PTY mode).
        let clean = strip_ansi_escapes(&line);
        let clean = clean.trim();
        if clean.is_empty() {
            continue;
        }

        // Try to parse as JSON. Lines that aren't JSON (shell errors,
        // stderr, escape-only chunks) are still streaming output the user
        // wants to see — surface them inline with an [exec] tag so nothing
        // is silently dropped.
        let json: serde_json::Value = match serde_json::from_str(clean) {
            Ok(v) => v,
            Err(_) => {
                if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n') {
                    state.agent_text.push('\n');
                }
                state.agent_text.push_str("[exec] ");
                state.agent_text.push_str(clean);
                state.agent_text.push('\n');
                if state.status == "starting" {
                    state.status = "running".to_string();
                    state.started_at = std::time::Instant::now();
                }
                continue;
            }
        };

        // Transition from "starting" once we receive any valid JSON.
        // Also reset the elapsed timer here so it measures agent execution
        // time, not VM boot + agent CLI init time (which can be ~30s).
        if state.status == "starting" {
            state.status = "running".to_string();
            state.started_at = std::time::Instant::now();
        }

        let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");

        // Trace every parsed event so we can diagnose missing-output bugs.
        // Truncated to keep logs readable for long content blocks.
        tracing::info!(
            "[parse_headless] event_type={:?} raw_len={} agent_text_len={}",
            event_type,
            clean.len(),
            state.agent_text.len()
        );

        match event_type {
            // =================================================================
            // Claude Code: -p --output-format stream-json (full SSE schema)
            // =================================================================

            // Granular SSE events from --include-partial-messages.
            // The Anthropic Messages stream nests its event family under .event.type:
            //   message_start, content_block_start, content_block_delta,
            //   content_block_stop, message_delta, message_stop, ping
            "stream_event" => {
                let inner_type = json
                    .pointer("/event/type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match inner_type {
                    "message_start" => {
                        if let Some(model) = json
                            .pointer("/event/message/model")
                            .and_then(|v| v.as_str())
                        {
                            state.status = format!("running ({})", model);
                        }
                    }
                    "content_block_start" => {
                        let block_type = json
                            .pointer("/event/content_block/type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        match block_type {
                            "tool_use" => {
                                let name = json
                                    .pointer("/event/content_block/name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("tool");
                                state.tool_calls.push(super::app::HeadlessToolCall {
                                    tool_name: name.to_string(),
                                    input_summary: String::new(),
                                    output_preview: String::new(),
                                    status: "running".to_string(),
                                });
                                state.status = "tool_use".to_string();
                            }
                            "thinking" => {
                                state.status = "thinking".to_string();
                            }
                            _ => {}
                        }
                    }
                    "content_block_delta" => {
                        let delta_type = json
                            .pointer("/event/delta/type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        match delta_type {
                            "text_delta" => {
                                if let Some(text) =
                                    json.pointer("/event/delta/text").and_then(|v| v.as_str())
                                {
                                    state.agent_text.push_str(text);
                                    state.status = "thinking".to_string();
                                }
                            }
                            "thinking_delta" => {
                                if let Some(text) = json
                                    .pointer("/event/delta/thinking")
                                    .and_then(|v| v.as_str())
                                {
                                    state.agent_text.push_str(text);
                                    state.status = "thinking".to_string();
                                }
                            }
                            "input_json_delta" => {
                                if let Some(partial) = json
                                    .pointer("/event/delta/partial_json")
                                    .and_then(|v| v.as_str())
                                {
                                    if let Some(last) = state.tool_calls.last_mut() {
                                        let mut combined = last.input_summary.clone();
                                        combined.push_str(partial);
                                        last.input_summary = truncate_str(&combined, 80);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    "content_block_stop" => {
                        if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n') {
                            state.agent_text.push('\n');
                        }
                    }
                    // Lifecycle markers — no payload to render.
                    "message_delta" | "message_stop" | "ping" => {}
                    // Legacy / un-nested form — preserve prior shallow extraction.
                    _ => {
                        if let Some(text) =
                            json.pointer("/event/delta/text").and_then(|v| v.as_str())
                        {
                            state.agent_text.push_str(text);
                            state.status = "thinking".to_string();
                        }
                        if let Some(name) = json
                            .pointer("/event/content_block/name")
                            .and_then(|v| v.as_str())
                        {
                            state.tool_calls.push(super::app::HeadlessToolCall {
                                tool_name: name.to_string(),
                                input_summary: String::new(),
                                output_preview: String::new(),
                                status: "running".to_string(),
                            });
                            state.status = "tool_use".to_string();
                        }
                    }
                }
            }
            // Complete assistant turn — content[] holds text, thinking, and tool_use blocks.
            "assistant" => {
                if let Some(content) = json
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for block in content {
                        let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        match block_type {
                            "text" => {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    state.agent_text.push_str(text);
                                    state.agent_text.push('\n');
                                }
                            }
                            "thinking" => {
                                if let Some(text) = block.get("thinking").and_then(|t| t.as_str()) {
                                    state.agent_text.push_str(text);
                                    state.agent_text.push('\n');
                                }
                            }
                            "tool_use" => {
                                let name = block
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("unknown");
                                let input = block
                                    .get("input")
                                    .map(|v| v.to_string())
                                    .unwrap_or_default();
                                let summary = truncate_str(&input, 200);
                                if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n')
                                {
                                    state.agent_text.push('\n');
                                }
                                state
                                    .agent_text
                                    .push_str(&format!("[tool:{}] {}\n", name, summary));
                                state.tool_calls.push(super::app::HeadlessToolCall {
                                    tool_name: name.to_string(),
                                    input_summary: truncate_str(&input, 80),
                                    output_preview: String::new(),
                                    status: "running".to_string(),
                                });
                                state.status = "tool_use".to_string();
                            }
                            _ => {
                                // Unknown content block — best-effort text extraction.
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    state.agent_text.push_str(text);
                                    state.agent_text.push('\n');
                                }
                            }
                        }
                    }
                }
            }
            // Claude final result. May indicate API failure via is_error/api_error_status
            // even when subtype="success" (Claude Code emits a synthetic result on auth
            // or quota errors, e.g. "Credit balance is too low" with HTTP 400).
            "result" => {
                let is_error = json
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let api_status = json.get("api_error_status").and_then(|v| v.as_i64());
                let result_text = json.get("result").and_then(|r| r.as_str()).unwrap_or("");

                if is_error || api_status.is_some() {
                    let header = match api_status {
                        Some(code) => format!("[error] API {}: ", code),
                        None => "[error] ".to_string(),
                    };
                    if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n') {
                        state.agent_text.push('\n');
                    }
                    state.agent_text.push_str(&header);
                    state.agent_text.push_str(result_text);
                    state.agent_text.push('\n');
                    tracing::info!(
                        "[parse_headless] result is_error=true api={:?} text={:?}",
                        api_status,
                        result_text
                    );
                    state.finish("error");
                } else {
                    if !result_text.is_empty() {
                        state.agent_text.push_str(result_text);
                    }
                    tracing::info!("[parse_headless] result success len={}", result_text.len());
                    state.finish("completed");
                }
            }

            // =================================================================
            // Codex: exec --json (NDJSON streaming)
            // =================================================================

            // Lifecycle events with no content payload.
            // Surface them as visible markers so the user sees activity.
            "thread.started" | "turn.started" => {
                if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n') {
                    state.agent_text.push('\n');
                }
                state.agent_text.push_str("[");
                state.agent_text.push_str(event_type);
                state.agent_text.push_str("]\n");
            }
            // End-of-turn — for one-shot `exec --json`, this signals run completion.
            "turn.completed" => {
                if let Some(last) = state.tool_calls.last_mut() {
                    if last.status == "running" {
                        last.status = "done".to_string();
                    }
                }
                state.finish("completed");
            }
            // Item events — the main content carriers.
            "item.started" | "item.updated" | "item.completed" => {
                if let Some(item) = json.get("item") {
                    let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match item_type {
                        "agent_message" | "reasoning" => {
                            // Text is at .item.text (NOT .item.content)
                            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                if event_type == "item.completed" {
                                    state.agent_text.push_str(text);
                                    state.agent_text.push('\n');
                                }
                            }
                            state.status = "thinking".to_string();
                        }
                        "command_execution" => {
                            if event_type == "item.started" {
                                let cmd = item
                                    .get("command")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("command");
                                if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n')
                                {
                                    state.agent_text.push('\n');
                                }
                                state.agent_text.push_str(&format!("$ {}\n", cmd));
                                state.tool_calls.push(super::app::HeadlessToolCall {
                                    tool_name: "command".to_string(),
                                    input_summary: truncate_str(cmd, 80),
                                    output_preview: String::new(),
                                    status: "running".to_string(),
                                });
                                state.status = "tool_use".to_string();
                            }
                            if event_type == "item.completed" {
                                let exit_code =
                                    item.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(0);
                                let output = item
                                    .get("aggregated_output")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if !output.is_empty() {
                                    state.agent_text.push_str(&truncate_str(output, 800));
                                    if !state.agent_text.ends_with('\n') {
                                        state.agent_text.push('\n');
                                    }
                                }
                                if exit_code != 0 {
                                    state
                                        .agent_text
                                        .push_str(&format!("[exit {}]\n", exit_code));
                                }
                                if let Some(last) = state.tool_calls.last_mut() {
                                    last.status = if exit_code == 0 {
                                        "done".to_string()
                                    } else {
                                        "error".to_string()
                                    };
                                    if !output.is_empty() {
                                        last.output_preview = truncate_str(output, 120);
                                    }
                                }
                            }
                        }
                        "file_change" => {
                            if event_type == "item.started" {
                                let path =
                                    item.get("path").and_then(|v| v.as_str()).unwrap_or("file");
                                if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n')
                                {
                                    state.agent_text.push('\n');
                                }
                                state.agent_text.push_str(&format!("[file] {}\n", path));
                                state.tool_calls.push(super::app::HeadlessToolCall {
                                    tool_name: "file_change".to_string(),
                                    input_summary: truncate_str(path, 80),
                                    output_preview: String::new(),
                                    status: "running".to_string(),
                                });
                                state.status = "tool_use".to_string();
                            }
                            if event_type == "item.completed" {
                                let summary = item
                                    .get("diff")
                                    .or_else(|| item.get("summary"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if !summary.is_empty() {
                                    state.agent_text.push_str(&truncate_str(summary, 400));
                                    if !state.agent_text.ends_with('\n') {
                                        state.agent_text.push('\n');
                                    }
                                }
                                if let Some(last) = state.tool_calls.last_mut() {
                                    last.status = "done".to_string();
                                    if !summary.is_empty() {
                                        last.output_preview = truncate_str(summary, 120);
                                    }
                                }
                            }
                        }
                        "mcp_tool_call" | "web_search" => {
                            if event_type == "item.started" {
                                let name = item
                                    .get("tool")
                                    .or_else(|| item.get("type"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("tool");
                                let input = item
                                    .get("arguments")
                                    .or_else(|| item.get("query"))
                                    .map(|v| v.to_string())
                                    .unwrap_or_default();
                                if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n')
                                {
                                    state.agent_text.push('\n');
                                }
                                state.agent_text.push_str(&format!(
                                    "[{}] {}\n",
                                    name,
                                    truncate_str(&input, 200)
                                ));
                                state.tool_calls.push(super::app::HeadlessToolCall {
                                    tool_name: name.to_string(),
                                    input_summary: truncate_str(&input, 80),
                                    output_preview: String::new(),
                                    status: "running".to_string(),
                                });
                                state.status = "tool_use".to_string();
                            }
                            if event_type == "item.completed" {
                                let output = item
                                    .get("result")
                                    .or_else(|| item.get("output"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if !output.is_empty() {
                                    state.agent_text.push_str(&format!(
                                        "[result] {}\n",
                                        truncate_str(output, 400)
                                    ));
                                }
                                if let Some(last) = state.tool_calls.last_mut() {
                                    last.status = "done".to_string();
                                    if !output.is_empty() {
                                        last.output_preview = truncate_str(output, 120);
                                    }
                                }
                            }
                        }
                        "todo_list" | "patch_apply" | "plan_update" => {
                            // Codex extension item types — surface as a tool entry.
                            if event_type == "item.started" {
                                let summary = item
                                    .get("summary")
                                    .or_else(|| item.get("path"))
                                    .or_else(|| item.get("title"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                state.tool_calls.push(super::app::HeadlessToolCall {
                                    tool_name: item_type.to_string(),
                                    input_summary: truncate_str(summary, 80),
                                    output_preview: String::new(),
                                    status: "running".to_string(),
                                });
                                state.status = "tool_use".to_string();
                            }
                            if event_type == "item.completed" {
                                if let Some(last) = state.tool_calls.last_mut() {
                                    last.status = "done".to_string();
                                }
                            }
                        }
                        _ => {
                            // Unknown subtype — render the raw type name so users see it.
                            if event_type == "item.started" && !item_type.is_empty() {
                                state.tool_calls.push(super::app::HeadlessToolCall {
                                    tool_name: item_type.to_string(),
                                    input_summary: String::new(),
                                    output_preview: String::new(),
                                    status: "running".to_string(),
                                });
                            }
                            if event_type == "item.completed" {
                                if let Some(last) = state.tool_calls.last_mut() {
                                    if last.status == "running" {
                                        last.status = "done".to_string();
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // =================================================================
            // Goose: run --output-format stream-json
            // =================================================================

            // Message event — assistant text, tool requests, and tool responses.
            "message" => {
                if let Some(content) = json
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for block in content {
                        let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        match block_type {
                            "text" => {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    state.agent_text.push_str(text);
                                    state.agent_text.push('\n');
                                    state.status = "thinking".to_string();
                                }
                            }
                            "tool_request" => {
                                let name = block
                                    .pointer("/tool_call/name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("tool");
                                let args = block
                                    .pointer("/tool_call/arguments")
                                    .map(|v| v.to_string())
                                    .unwrap_or_default();
                                if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n')
                                {
                                    state.agent_text.push('\n');
                                }
                                state.agent_text.push_str(&format!(
                                    "[tool:{}] {}\n",
                                    name,
                                    truncate_str(&args, 200)
                                ));
                                state.tool_calls.push(super::app::HeadlessToolCall {
                                    tool_name: name.to_string(),
                                    input_summary: truncate_str(&args, 80),
                                    output_preview: String::new(),
                                    status: "running".to_string(),
                                });
                                state.status = "tool_use".to_string();
                            }
                            "tool_response" => {
                                let is_error = block
                                    .get("is_error")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                if let Some(last) = state.tool_calls.last_mut() {
                                    last.status = if is_error {
                                        "error".to_string()
                                    } else {
                                        "done".to_string()
                                    };
                                    // Goose response payloads can live in tool_result[],
                                    // content[], or a flat output string.
                                    let mut output = String::new();
                                    if let Some(arr) =
                                        block.pointer("/tool_result").and_then(|v| v.as_array())
                                    {
                                        for c in arr {
                                            if let Some(t) = c.get("text").and_then(|v| v.as_str())
                                            {
                                                output.push_str(t);
                                            }
                                        }
                                    } else if let Some(arr) =
                                        block.get("content").and_then(|v| v.as_array())
                                    {
                                        for c in arr {
                                            if let Some(t) = c.get("text").and_then(|v| v.as_str())
                                            {
                                                output.push_str(t);
                                            }
                                        }
                                    } else if let Some(s) =
                                        block.get("output").and_then(|v| v.as_str())
                                    {
                                        output.push_str(s);
                                    } else if let Some(s) =
                                        block.get("content").and_then(|v| v.as_str())
                                    {
                                        output.push_str(s);
                                    }
                                    if !output.is_empty() {
                                        last.output_preview = truncate_str(&output, 120);
                                    }
                                }
                                // Surface response inline regardless of tool_calls state.
                                let mut output = String::new();
                                if let Some(arr) =
                                    block.pointer("/tool_result").and_then(|v| v.as_array())
                                {
                                    for c in arr {
                                        if let Some(t) = c.get("text").and_then(|v| v.as_str()) {
                                            output.push_str(t);
                                        }
                                    }
                                } else if let Some(arr) =
                                    block.get("content").and_then(|v| v.as_array())
                                {
                                    for c in arr {
                                        if let Some(t) = c.get("text").and_then(|v| v.as_str()) {
                                            output.push_str(t);
                                        }
                                    }
                                } else if let Some(s) = block.get("output").and_then(|v| v.as_str())
                                {
                                    output.push_str(s);
                                } else if let Some(s) =
                                    block.get("content").and_then(|v| v.as_str())
                                {
                                    output.push_str(s);
                                }
                                let tag = if is_error {
                                    "[result:error]"
                                } else {
                                    "[result]"
                                };
                                let display = if output.is_empty() {
                                    "(empty)"
                                } else {
                                    output.as_str()
                                };
                                if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n')
                                {
                                    state.agent_text.push('\n');
                                }
                                state.agent_text.push_str(&format!(
                                    "{} {}\n",
                                    tag,
                                    truncate_str(display, 400)
                                ));
                            }
                            "image" => {
                                state.agent_text.push_str("[image]\n");
                            }
                            _ => {}
                        }
                    }
                }
            }
            // Notification — extension log / progress messages with severity.
            "notification" => {
                let msg = json
                    .pointer("/log/message")
                    .or_else(|| json.pointer("/progress/message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let level = json
                    .pointer("/log/level")
                    .and_then(|v| v.as_str())
                    .unwrap_or("info");
                let ext = json
                    .get("extension_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("ext");
                if !msg.is_empty() {
                    if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n') {
                        state.agent_text.push('\n');
                    }
                    state.agent_text.push_str(&format!(
                        "[{}:{}] {}\n",
                        ext,
                        level,
                        truncate_str(msg, 200)
                    ));
                    state.tool_calls.push(super::app::HeadlessToolCall {
                        tool_name: ext.to_string(),
                        input_summary: truncate_str(msg, 80),
                        output_preview: String::new(),
                        status: if level == "error" || level == "warn" {
                            "error".to_string()
                        } else {
                            "done".to_string()
                        },
                    });
                }
            }
            // Goose completion / cancellation.
            "complete" => {
                if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n') {
                    state.agent_text.push('\n');
                }
                state.agent_text.push_str("[done]\n");
                state.finish("completed");
            }
            "cancelled" => {
                state.agent_text.push_str("[cancelled]\n");
                state.finish("error");
            }

            // =================================================================
            // Cursor: -p --output-format stream-json
            // =================================================================

            // Thinking — token-level reasoning stream.
            "thinking" => {
                let subtype = json.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
                match subtype {
                    "started" => {
                        state.status = "thinking".to_string();
                    }
                    "delta" => {
                        if let Some(text) = json.get("text").and_then(|v| v.as_str()) {
                            state.agent_text.push_str(text);
                        }
                        state.status = "thinking".to_string();
                    }
                    "completed" => {
                        if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n') {
                            state.agent_text.push('\n');
                        }
                    }
                    _ => {}
                }
            }

            // Tool call lifecycle — tool name is the KEY inside the `tool_call` object.
            // e.g. {"tool_call": {"shellToolCall": {"args": {"command": "ls"}}}}
            "tool_call" => {
                let subtype = json.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
                let tc_obj = json.get("tool_call").and_then(|v| v.as_object()).cloned();
                if subtype == "started" {
                    let (name, input) = if let Some(ref obj) = tc_obj {
                        let key = obj.keys().next().map(|k| k.as_str()).unwrap_or("tool");
                        let friendly = key.strip_suffix("ToolCall").unwrap_or(key);
                        let args = obj
                            .values()
                            .next()
                            .and_then(|v| v.get("args"))
                            .and_then(|a| a.as_object());
                        let input = args
                            .and_then(|a| {
                                a.get("command")
                                    .or_else(|| a.get("path"))
                                    .or_else(|| a.get("query"))
                                    .or_else(|| a.get("pattern"))
                                    .or_else(|| a.get("url"))
                                    .or_else(|| a.get("file_path"))
                                    .and_then(|v| v.as_str())
                            })
                            .unwrap_or("");
                        (friendly.to_string(), input.to_string())
                    } else {
                        ("tool".to_string(), String::new())
                    };
                    if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n') {
                        state.agent_text.push('\n');
                    }
                    state.agent_text.push_str(&format!(
                        "[tool:{}] {}\n",
                        name,
                        truncate_str(&input, 200)
                    ));
                    state.tool_calls.push(super::app::HeadlessToolCall {
                        tool_name: name,
                        input_summary: truncate_str(&input, 80),
                        output_preview: String::new(),
                        status: "running".to_string(),
                    });
                    state.status = "tool_use".to_string();
                } else if subtype == "completed" {
                    let mut found_output: Option<(String, bool)> = None;
                    if let Some(ref obj) = tc_obj {
                        if let Some((_, val)) = obj.iter().next() {
                            let candidates = [
                                ("/result/success/output", false),
                                ("/result/success/content", false),
                                ("/result/success/text", false),
                                ("/result/success/result", false),
                                ("/result/success/message", false),
                                ("/result/error/output", true),
                                ("/result/error/message", true),
                            ];
                            for (path, is_err) in candidates.iter() {
                                if let Some(out) = val.pointer(path).and_then(|v| v.as_str()) {
                                    found_output = Some((out.to_string(), *is_err));
                                    break;
                                }
                            }
                        }
                    }
                    if let Some((ref out, is_err)) = found_output {
                        let tag = if is_err { "[result:error]" } else { "[result]" };
                        if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n') {
                            state.agent_text.push('\n');
                        }
                        state
                            .agent_text
                            .push_str(&format!("{} {}\n", tag, truncate_str(out, 400)));
                    }
                    if let Some(last) = state.tool_calls.last_mut() {
                        last.status = "done".to_string();
                        if let Some((out, is_err)) = found_output {
                            last.output_preview = truncate_str(&out, 120);
                            if is_err {
                                last.status = "error".to_string();
                            }
                        }
                    }
                } else if subtype == "failed" || subtype == "error" {
                    if let Some(last) = state.tool_calls.last_mut() {
                        last.status = "error".to_string();
                        let err = json
                            .pointer("/error/message")
                            .or_else(|| json.get("error"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if !err.is_empty() {
                            last.output_preview = truncate_str(err, 120);
                        }
                    }
                }
            }

            // =================================================================
            // Lifecycle and system events
            // =================================================================

            // System init — extract model info for display and surface inline.
            "system" => {
                let subtype = json.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
                let model = json.get("model").and_then(|v| v.as_str()).unwrap_or("");
                let cwd = json.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
                if !model.is_empty() {
                    state.status = format!("running ({})", model);
                }
                let mut header = String::from("[system");
                if !subtype.is_empty() {
                    header.push(':');
                    header.push_str(subtype);
                }
                header.push(']');
                if !model.is_empty() {
                    header.push_str(" model=");
                    header.push_str(model);
                }
                if !cwd.is_empty() {
                    header.push_str(" cwd=");
                    header.push_str(cwd);
                }
                if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n') {
                    state.agent_text.push('\n');
                }
                state.agent_text.push_str(&header);
                state.agent_text.push('\n');
            }

            // Tool result events (Claude `user` with tool_result blocks).
            // The result content can be a string, an array of {type,text} blocks,
            // or absent entirely (legacy form — just acknowledge tool done).
            "user" => {
                let content = json
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array());
                if let Some(blocks) = content {
                    for block in blocks {
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                            let is_error = block
                                .get("is_error")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            let mut output = String::new();
                            if let Some(s) = block.get("content").and_then(|v| v.as_str()) {
                                output.push_str(s);
                            } else if let Some(arr) =
                                block.get("content").and_then(|v| v.as_array())
                            {
                                for c in arr {
                                    if let Some(t) = c.get("text").and_then(|v| v.as_str()) {
                                        output.push_str(t);
                                    }
                                }
                            }
                            // Surface the tool result inline in the panel.
                            let tag = if is_error {
                                "[result:error]"
                            } else {
                                "[result]"
                            };
                            let display = if output.is_empty() {
                                "(empty)"
                            } else {
                                output.as_str()
                            };
                            if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n') {
                                state.agent_text.push('\n');
                            }
                            state.agent_text.push_str(&format!(
                                "{} {}\n",
                                tag,
                                truncate_str(display, 400)
                            ));
                            if let Some(last) = state.tool_calls.last_mut() {
                                last.status = if is_error {
                                    "error".to_string()
                                } else {
                                    "done".to_string()
                                };
                                if !output.is_empty() {
                                    last.output_preview = truncate_str(&output, 120);
                                }
                            }
                        }
                    }
                } else if let Some(last) = state.tool_calls.last_mut() {
                    if last.status == "running" {
                        last.status = "done".to_string();
                    }
                }
            }

            // Errors — surface as visible text so the user sees them.
            "error" | "turn.failed" => {
                let msg = json
                    .pointer("/error/message")
                    .and_then(|v| v.as_str())
                    .or_else(|| json.get("error").and_then(|v| v.as_str()))
                    .or_else(|| json.get("message").and_then(|v| v.as_str()))
                    .unwrap_or("unknown error");
                let code = json
                    .pointer("/error/code")
                    .and_then(|v| v.as_str())
                    .or_else(|| json.pointer("/error/type").and_then(|v| v.as_str()));
                let header = match code {
                    Some(c) => format!("[error {}] ", c),
                    None => "[error] ".to_string(),
                };
                if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n') {
                    state.agent_text.push('\n');
                }
                state.agent_text.push_str(&header);
                state.agent_text.push_str(msg);
                state.agent_text.push('\n');
                state.finish("error");
            }

            // Model change — surface inline, others are too chatty to render.
            "model_change" => {
                let model = json
                    .get("model")
                    .or_else(|| json.get("to"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n') {
                    state.agent_text.push('\n');
                }
                state.agent_text.push_str("[model_change] ");
                state.agent_text.push_str(model);
                state.agent_text.push('\n');
                if !model.is_empty() {
                    state.status = format!("running ({})", model);
                }
            }

            // Truly silent lifecycle events — no payload, very chatty.
            "ping" | "keepalive" | "heartbeat" | "session.created" | "session.updated"
            | "usage" | "rate_limit" => {}

            _ => {
                // Unknown event — try several common text-bearing fields.
                let extracted = json
                    .get("text")
                    .and_then(|v| v.as_str())
                    .or_else(|| json.pointer("/delta/text").and_then(|v| v.as_str()))
                    .or_else(|| json.pointer("/data/text").and_then(|v| v.as_str()))
                    .or_else(|| json.get("content").and_then(|v| v.as_str()))
                    .or_else(|| json.get("output").and_then(|v| v.as_str()));
                if let Some(text) = extracted {
                    state.agent_text.push_str(text);
                }
            }
        }
    }
}

/// Strip ANSI escape sequences from a string.
fn strip_ansi_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // CSI sequence: ESC [ ... final_byte
            if let Some(next) = chars.next() {
                if next == '[' {
                    // Consume until we hit a letter (@ through ~)
                    for seq_char in chars.by_ref() {
                        if seq_char.is_ascii_alphabetic() || seq_char == '~' {
                            break;
                        }
                    }
                }
                // OSC, other escape types — skip until BEL or ST
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...", &s[..max])
    } else {
        s.to_string()
    }
}

fn detect_agent_type_from_image(image: &str) -> Option<String> {
    // Get the last path segment first, then strip the tag.
    // This avoids confusing registry port (localhost:5050) with tag separator.
    let last_segment = image.rsplit('/').next().unwrap_or(image);
    // Strip tag (the part after the last colon in the segment)
    let image_name = last_segment.split(':').next().unwrap_or(last_segment);

    for agent in KNOWN_AGENTS {
        // Match patterns: "agent-claude", "nanosb-claude", "claude", etc.
        if image_name == *agent
            || image_name.ends_with(&format!("-{}", agent))
            || image_name.starts_with(&format!("{}-", agent))
        {
            return Some(agent.to_string());
        }
    }
    None
}

/// Required API key environment variables for known agents.
fn required_api_keys(agent: &str) -> Vec<(&'static str, bool)> {
    match agent {
        "claude" => vec![("ANTHROPIC_API_KEY", true)],
        "codex" => vec![("OPENAI_API_KEY", true)],
        "goose" => vec![("OPENAI_API_KEY", false), ("ANTHROPIC_API_KEY", false)],
        _ => vec![],
    }
}

fn parse_runtime_env_file(path: &str) -> std::result::Result<Vec<(String, String)>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read env file '{}': {}", path, e))?;

    let mut vars = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            vars.push((key.trim().to_string(), value.trim().to_string()));
        }
    }

    Ok(vars)
}

/// Add a new agent panel, creating and starting a sandbox in the background.
fn add_agent(
    app: &mut App,
    agent: &str,
    image: Option<&str>,
    tag: Option<&str>,
    project: Option<&str>,
    branch: Option<&str>,
    name: Option<&str>,
    auto_mode: bool,
    interactive: bool,
    prompt: Option<&str>,
    model: Option<&str>,
    use_env_keys: &[String],
    env_file: Option<&str>,
    run_as_root: bool,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    // Build image reference. --tag overrides the default "latest" tag
    // for bare agent names (e.g., `/add claude --tag rc11` → `claude:rc11`).
    let image_ref = match image {
        Some(img) => img.to_string(),
        None => match tag {
            Some(t) => format!("{}:{}", agent, t),
            None => agent.to_string(),
        },
    };
    let image_name = sandbox::normalize_image(&image_ref);

    let mut panel = AgentPanel::new(agent);

    // Headless mode setup.
    panel.auto_mode = auto_mode;
    panel.interactive = interactive && !auto_mode;
    if auto_mode {
        panel.permissions = sandbox::Permissions::AllowAll;
        let task = prompt.unwrap_or("(no prompt)");
        panel.headless_state = Some(super::app::HeadlessState::new(task));
    }

    // Import startup runtime env pool by default (lowest precedence).
    for (key, value) in &app.runtime_env_pool {
        panel.env.insert(key.clone(), value.clone());
        panel.runtime_env_keys.insert(key.clone());
    }

    // Merge per-panel --env-file values over runtime defaults.
    if let Some(env_file_path) = env_file {
        match parse_runtime_env_file(env_file_path) {
            Ok(file_vars) => {
                for (key, value) in file_vars {
                    panel.runtime_env_keys.insert(key.clone());
                    panel.env.insert(key, value);
                }
            }
            Err(e) => {
                app.set_system_message(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Cannot add panel: {}", e),
                });
                return;
            }
        }
    }

    // Auto-detect API keys from host environment.
    for (key, _is_required) in &required_api_keys(agent) {
        if let Ok(val) = std::env::var(key) {
            panel.env.entry(key.to_string()).or_insert(val);
        }
    }

    // Goose requires GOOSE_PROVIDER to be set — auto-detect from available keys.
    if agent == "goose" && !panel.env.contains_key("GOOSE_PROVIDER") {
        if panel.env.contains_key("ANTHROPIC_API_KEY") {
            panel
                .env
                .insert("GOOSE_PROVIDER".to_string(), "anthropic".to_string());
        } else if panel.env.contains_key("OPENAI_API_KEY") {
            panel
                .env
                .insert("GOOSE_PROVIDER".to_string(), "openai".to_string());
        }
    }

    // Explicit runtime key picks override all inferred/default values.
    if !use_env_keys.is_empty() {
        let mut missing: Vec<String> = Vec::new();
        for key in use_env_keys {
            if let Some(value) = app.runtime_env_pool.get(key) {
                panel.env.insert(key.clone(), value.clone());
                panel.runtime_env_keys.insert(key.clone());
            } else {
                missing.push(key.clone());
            }
        }

        if !missing.is_empty() {
            app.set_system_message(ChatMessage {
                role: MessageRole::System,
                content: format!(
                    "Cannot add panel: runtime env key(s) not found: {}",
                    missing.join(", ")
                ),
            });
            return;
        }
    }

    // Set model selection.
    panel.model = model.map(String::from);

    // Build sandbox config.
    // Compute defaults are loaded from built-in agent profiles (and optional
    // user overrides in ~/.nanosandbox/agent_defaults.yaml).
    let project_path = project
        .map(std::path::PathBuf::from)
        .or_else(|| app.project_path.clone());

    // Resolve agent-type-aware compute defaults for cpus and memory.
    let parsed_agent_type = agent.parse::<sandbox::AgentType>().ok();
    let compute = parsed_agent_type.map(sandbox::agent_compute_for).unwrap_or(
        sandbox::AgentComputeDefaults {
            cpus: 2,
            memory_mb: 2048,
        },
    );

    let mut builder = SandboxConfig::builder()
        .image(&image_name)
        .cpus(compute.cpus)
        .memory_mb(compute.memory_mb)
        .run_as_root(run_as_root);

    // Set agent type on panel (not on SandboxConfig builder).
    if let Some(agent_type) = parsed_agent_type {
        panel.agent_type = Some(agent_type);
    }

    if let Some(n) = name {
        builder = builder.name(n);
    }

    if let Some(ref pp) = project_path {
        builder = builder.project(pp, branch);
    }

    let mut config = builder.build();

    // Propagate the resolved agent type onto the config so downstream deploy
    // planning (`deploy_plan_for` -> `has_agent`) actually builds the agent
    // command. Without this the VM boots with no command (sleep hold).
    if let Some(agent_type) = panel.agent_type {
        config.agent_type = Some(agent_type);
    }
    config.interactive = panel.interactive;

    // Pass auto_sync setting to project config so sandbox creation
    // knows whether to use setup() or setup_deferred().
    if let Some(ref mut proj) = config.sandbox.project {
        proj.auto_sync = app.settings.gitsync.auto_sync;
    }

    // Store the config for session persistence.
    panel.original_config = Some(config.clone());
    panel.display_name = name.map(String::from);

    app.panels.push(panel);
    let panel_idx = app.panels.len() - 1;
    app.focused_panel = panel_idx;
    app.show_welcome = false;
    app.focus_panel_input();

    // Spawn sandbox creation in the background so the event loop stays responsive.
    let tx = tx.clone();
    tokio::spawn(async move {
        if try_attach_supervisor(&config.sandbox.name, panel_idx, &tx) {
            return;
        }
        spawn_supervisor_panel(config, panel_idx, &tx).await;
    });
}

/// Add an agent panel from a resolved AgentSandboxConfig (from sandbox.yml).
// Visibility: pub(crate) so the #[cfg(test)] vm_test module can call it.
pub(crate) fn add_agent_from_config(
    app: &mut App,
    key: &str,
    mut config: AgentSandboxConfig,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    let display_name = config.sandbox.name.clone();

    // Detect the base agent type: explicit config.agent_type > image name > key.
    let agent_type = config
        .agent_type
        .map(|t| t.to_string())
        .or_else(|| detect_agent_type_from_image(&config.sandbox.image))
        .unwrap_or_else(|| key.to_string());

    let mut panel = AgentPanel::new(&agent_type);
    panel.display_name = Some(display_name.clone());
    panel.auto_mode = config.auto_mode;
    panel.interactive = config.interactive && !config.auto_mode;
    panel.permissions = config.permissions;
    panel.model = config.model.clone();
    if config.auto_mode {
        let task = config.prompt.as_deref().unwrap_or("(no prompt)");
        panel.headless_state = Some(super::app::HeadlessState::new(task));
    }

    // Propagate the detected agent type onto the config so deploy planning
    // builds the agent command (see add_agent for the same fix).
    if config.agent_type.is_none() {
        if let Ok(at) = agent_type.parse::<sandbox::AgentType>() {
            config.agent_type = Some(at);
        }
    }

    // Copy env vars from config to panel.
    for (k, v) in &config.sandbox.env {
        panel.env.insert(k.clone(), v.clone());
    }

    // Import startup runtime env pool as fallback only (config keys win).
    for (key, value) in &app.runtime_env_pool {
        if !panel.env.contains_key(key) {
            panel.env.insert(key.clone(), value.clone());
            panel.runtime_env_keys.insert(key.clone());
        }
    }

    // Auto-detect API keys from host environment (if not already in config env).
    for (api_key, _) in &required_api_keys(&agent_type) {
        if !panel.env.contains_key(*api_key) {
            if let Ok(val) = std::env::var(api_key) {
                panel.env.insert(api_key.to_string(), val);
            }
        }
    }

    // If config doesn't have a project but --project flag was set,
    // inject it into the config so the sandbox mounts it.
    if config.sandbox.project.is_none() {
        if let Some(ref project_path) = app.project_path {
            config.sandbox.project = Some(sandbox::ProjectConfig {
                path: project_path.clone(),
                branch: None,
                mount_point: "/workspace".to_string(),
                auto_sync: app.settings.gitsync.auto_sync,
            });
        }
    } else {
        // Config has a project — just set auto_sync from settings.
        if let Some(ref mut proj) = config.sandbox.project {
            proj.auto_sync = app.settings.gitsync.auto_sync;
        }
    }

    // Store the runtime config for session persistence.
    panel.original_config = Some(config.clone());

    app.panels.push(panel);
    let panel_idx = app.panels.len() - 1;
    app.focused_panel = panel_idx;
    app.show_welcome = false;
    app.focus_panel_input();

    let tx = tx.clone();
    tokio::spawn(async move {
        if try_attach_supervisor(&config.sandbox.name, panel_idx, &tx) {
            return;
        }
        spawn_supervisor_panel(config, panel_idx, &tx).await;
    });
}
fn normalize_agent_name(agent_name: &str) -> &str {
    match agent_name {
        "claude-code" => "claude",
        "cursor-agent" => "cursor",
        _ => agent_name,
    }
}

fn extract_json_string_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\"", key);
    let idx = text.find(&needle)?;
    let after_key = text.get(idx + needle.len()..)?;
    let colon_idx = after_key.find(':')?;
    let after_colon = after_key.get(colon_idx + 1..)?.trim_start();
    if !after_colon.starts_with('"') {
        return None;
    }
    let mut out = String::new();
    let mut escaped = false;
    for c in after_colon[1..].chars() {
        if escaped {
            out.push(c);
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == '"' {
            break;
        }
        out.push(c);
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn collect_state_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                files.push(path);
            }
        }
    }

    files.sort_by(|a, b| {
        let ma = std::fs::metadata(a)
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let mb = std::fs::metadata(b)
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        mb.cmp(&ma)
    });
    files
}

fn detect_goose_session_id_from_state(
    clone_path: &std::path::Path,
) -> std::result::Result<Option<String>, String> {
    let db_path = clone_path.join(".nanosb-state/.config/goose/data/sessions/sessions.db");
    if !db_path.exists() {
        return Ok(None);
    }

    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| {
        format!(
            "failed to open goose sessions DB ({}): {}",
            db_path.display(),
            e
        )
    })?;

    match conn.query_row(
        "SELECT id FROM sessions ORDER BY created_at DESC LIMIT 1",
        [],
        |row| row.get::<_, String>(0),
    ) {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(format!(
            "failed to query goose sessions DB ({}): {}",
            db_path.display(),
            e
        )),
    }
}

fn detect_agent_session_id_from_state(
    agent_name: &str,
    clone_path: &std::path::Path,
) -> std::result::Result<Option<String>, String> {
    if normalize_agent_name(agent_name) == "goose" {
        return detect_goose_session_id_from_state(clone_path);
    }

    let state_root = clone_path.join(".nanosb-state");
    if !state_root.exists() {
        return Ok(None);
    }

    let agent_dir = match normalize_agent_name(agent_name) {
        "claude" => state_root.join(".claude"),
        "codex" => state_root.join(".codex"),
        "cursor" => state_root.join(".cursor"),
        _ => state_root,
    };

    if !agent_dir.exists() {
        return Ok(None);
    }

    let files = collect_state_files(&agent_dir);
    for path in files.iter().take(250) {
        let content = std::fs::read_to_string(path).unwrap_or_default();
        if content.is_empty() {
            continue;
        }

        let by_json = match normalize_agent_name(agent_name) {
            "claude" => extract_json_string_field(&content, "session_id")
                .or_else(|| extract_json_string_field(&content, "sessionId")),
            "cursor" => extract_json_string_field(&content, "chatId")
                .or_else(|| extract_json_string_field(&content, "session_id"))
                .or_else(|| extract_json_string_field(&content, "sessionId")),
            _ => extract_json_string_field(&content, "session_id")
                .or_else(|| extract_json_string_field(&content, "sessionId")),
        };
        if by_json.is_some() {
            return Ok(by_json);
        }
    }

    Ok(None)
}

fn should_resume_panel(had_interaction: bool, selected_agent_session_id: Option<&str>) -> bool {
    had_interaction && selected_agent_session_id.is_some()
}

/// Resume panels from a saved session.
///
/// For each panel in the session, this creates a fresh sandbox with the saved config
/// but reuses the existing project clone (if still present). The agent is launched
/// with an explicit native session id when available.
fn resume_session(
    app: &mut App,
    session: &sandbox::session::Session,
    tx: &mpsc::UnboundedSender<AppEvent>,
) -> std::result::Result<(), String> {
    for sp in &session.panels {
        let mut config = sp.config.clone();

        // Normalize the image in case the session was saved with a bare name.
        config.sandbox.image = sandbox::normalize_image(&config.sandbox.image);

        // Set up the agent panel.
        let agent_type = detect_agent_type_from_image(&config.sandbox.image)
            .unwrap_or_else(|| sp.agent_name.clone());

        let mut panel = AgentPanel::new(&agent_type);
        panel.display_name = sp.display_name.clone();
        panel.auto_mode = sp.auto_mode;
        panel.permissions = sp.permissions;
        if sp.auto_mode {
            let task = sp.prompt.as_deref().unwrap_or("(no prompt)");
            panel.headless_state = Some(super::app::HeadlessState::new(task));
        }
        panel.visible = sp.visible;
        panel.agent_type = sp.agent_type;
        panel.model = sp.model.clone();
        panel.original_config = Some(config.clone());

        // Copy env vars from config to panel.
        for (k, v) in &config.sandbox.env {
            panel.env.insert(k.clone(), v.clone());
        }

        // Re-populate saved env keys from host env first, then startup runtime pool.
        // Values are runtime-only and are not copied back into persisted config.
        for key in &sp.env_keys {
            if let Ok(val) = std::env::var(key) {
                panel.env.insert(key.clone(), val);
            } else if let Some(val) = app.runtime_env_pool.get(key) {
                panel.env.insert(key.clone(), val.clone());
                panel.runtime_env_keys.insert(key.clone());
            }
        }

        // Auto-detect API keys from host environment.
        for (api_key, _) in &required_api_keys(&agent_type) {
            if !panel.env.contains_key(*api_key) {
                if let Ok(val) = std::env::var(api_key) {
                    panel.env.insert(api_key.to_string(), val);
                }
            }
        }

        // If the project clone still exists, mount it directly instead of
        // creating a new one. We set config.sandbox.project = None so that
        // setup_project_mount() inside Sandbox::create() is skipped, and add
        // the VirtioFS mount for the existing clone ourselves.
        //
        // Use the per-panel project path from the saved config (which may
        // differ from the global session.project_path when `/add --project`
        // pointed to a different directory).
        let panel_project_path = sp
            .config
            .sandbox
            .project
            .as_ref()
            .map(|p| p.path.clone())
            .unwrap_or_else(|| session.project_path.clone());

        if let Some(ref clone_path) = sp.clone_path {
            if clone_path.exists() {
                // Add VirtioFS mount for existing clone directly.
                config
                    .sandbox
                    .mounts
                    .push(sandbox::Mount::virtiofs(clone_path, "/workspace"));
                // Do NOT set config.sandbox.project — this skips setup_project_mount().
                config.sandbox.project = None;

                // Create a ProjectMount on the panel to handle suspend/teardown.
                if let Ok(mut pm) = sandbox::ProjectMount::detect(&panel_project_path) {
                    let _ = pm.resume(clone_path, sp.branches.clone());
                    panel.project_mount = Some(pm);
                }
            } else {
                // Clone dir is gone — create a fresh clone from the branch.
                config.sandbox.project = Some(sandbox::ProjectConfig {
                    path: panel_project_path,
                    branch: sp.branches.first().map(|(_, b)| b.clone()),
                    mount_point: "/workspace".to_string(),
                    auto_sync: app.settings.gitsync.auto_sync,
                });
            }
        }

        let detected_session_id = if let Some(clone) = panel
            .project_mount
            .as_ref()
            .and_then(|pm| pm.worktree_base.as_ref())
        {
            detect_agent_session_id_from_state(&agent_type, clone).map_err(|e| {
                format!(
                    "failed to detect session id for agent '{}' from '{}': {}",
                    agent_type,
                    clone.display(),
                    e
                )
            })?
        } else {
            None
        };

        panel.selected_agent_session_id =
            sp.selected_agent_session_id.clone().or(detected_session_id);

        // Resume only when we have a deterministic agent-native session id.
        // This avoids generic continue flags resuming an unrelated conversation.
        panel.is_resumed = should_resume_panel(
            sp.had_interaction,
            panel.selected_agent_session_id.as_deref(),
        );

        app.panels.push(panel);
        let panel_idx = app.panels.len() - 1;
        app.focused_panel = panel_idx;
        app.show_welcome = false;
        app.focus_panel_input();

        // Spawn sandbox creation in background.
        let tx = tx.clone();
        tokio::spawn(async move {
            if try_attach_supervisor(&config.sandbox.name, panel_idx, &tx) {
                return;
            }
            spawn_supervisor_panel(config, panel_idx, &tx).await;
        });
    }

    Ok(())
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
                app.set_system_message(ChatMessage {
                    role: MessageRole::System,
                    content: "No panel focused. Use /add <agent> first.".to_string(),
                });
            }
        }
        Some((key, value)) => {
            if let Some(panel) = app.focused_panel_mut() {
                panel.runtime_env_keys.remove(&key);
                panel.env.insert(key.clone(), value);
                panel.chat_history.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Set {}.", key),
                });
            } else {
                app.set_system_message(ChatMessage {
                    role: MessageRole::System,
                    content: "No panel focused. Use /add <agent> first.".to_string(),
                });
            }
        }
    }
}

/// Handle `/mcp list` — show MCP servers from sandbox.yml config.
async fn handle_mcp_list(app: &mut App) {
    let panel = match app.focused_panel_ref() {
        Some(p) => p,
        None => {
            app.set_system_message_persistent(ChatMessage {
                role: MessageRole::System,
                content: "No panel focused.".to_string(),
            });
            return;
        }
    };

    let config = match panel.original_config.as_ref() {
        Some(c) => c,
        None => {
            app.set_system_message_persistent(ChatMessage {
                role: MessageRole::System,
                content: "No sandbox config available.".to_string(),
            });
            return;
        }
    };

    let servers = &config.mcp_servers;
    let content = if servers.is_empty() {
        "No MCP servers configured in sandbox.yml.\n\
         Add them to sandbox.yml and redeploy (nanosb apply)."
            .to_string()
    } else {
        let mut lines = vec!["MCP Servers (from sandbox.yml):".to_string()];
        let mut sorted: Vec<_> = servers.iter().collect();
        sorted.sort_by_key(|(n, _)| n.as_str());
        for (name, cfg) in sorted {
            let status = if cfg.enabled { "enabled" } else { "disabled" };
            lines.push(format!(
                "  {} [{}]  {} {}",
                name,
                status,
                cfg.command,
                cfg.args.join(" "),
            ));
        }
        lines.push(String::new());
        lines.push("Changes require redeploy: edit sandbox.yml and run nanosb apply.".to_string());
        lines.join("\n")
    };
    app.set_system_message_persistent(ChatMessage {
        role: MessageRole::System,
        content,
    });
}

// ========== Agents Registry Loader ==========

/// Try to load the agents registry from well-known locations.
///
/// Search order:
/// 1. `NANOSB_REGISTRY_PATH` environment variable
/// 2. `~/.nanosandbox/agents-registry/`
/// 3. `../agents-registry/` (sibling directory — dev setup)
/// Build a session snapshot from the current TUI application state.
///
/// This replaces `Session::from_app()` which lived in the runtime crate and
/// depended on TUI types. Since `Session` fields are all public, we construct
/// it directly.
fn build_session_from_app(
    app: &super::app::App,
    project_path: &std::path::Path,
    sandbox_config_content: &str,
) -> std::result::Result<sandbox::session::Session, String> {
    use chrono::Utc;
    use sandbox::session::{config_hash, Session, SessionPanel, SESSION_VERSION};

    let mut panels: Vec<SessionPanel> = Vec::new();
    for panel in &app.panels {
        let Some(config) = panel.original_config.clone() else {
            continue;
        };

        let clone_path = panel
            .project_mount
            .as_ref()
            .and_then(|pm| pm.worktree_base.clone());

        let branches = panel
            .project_mount
            .as_ref()
            .map(|pm| pm.created_branches.clone())
            .unwrap_or_default();

        let env_keys: Vec<String> = panel
            .env
            .keys()
            .filter(|k| !panel.runtime_env_keys.contains(*k))
            .cloned()
            .collect();

        let selected_agent_session_id =
            if let Some(existing) = panel.selected_agent_session_id.clone() {
                Some(existing)
            } else if let Some(ref clone) = clone_path {
                detect_agent_session_id_from_state(&panel.agent_name, clone).map_err(|e| {
                    format!(
                        "failed to detect session id for agent '{}' from '{}': {}",
                        panel.agent_name,
                        clone.display(),
                        e
                    )
                })?
            } else {
                None
            };

        panels.push(SessionPanel {
            agent_name: panel.agent_name.clone(),
            display_name: panel.display_name.clone(),
            sandbox_short_id: panel.sandbox_id_short.clone(),
            config,
            clone_path,
            branches,
            auto_mode: panel.auto_mode,
            permissions: panel.permissions,
            agent_type: panel.agent_type,
            model: panel.model.clone(),
            prompt: panel.headless_state.as_ref().map(|h| h.task.clone()),
            env_keys,
            visible: panel.visible,
            had_interaction: panel.had_interaction,
            selected_agent_session_id,
        });
    }

    Ok(Session {
        version: SESSION_VERSION,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        project_path: project_path.to_path_buf(),
        config_hash: config_hash(sandbox_config_content),
        panels,
    })
}

/// Merge registry resolution (full agent or skills-only) into sandbox config for bootstrap.
fn merge_resolved_agent_into_config(
    config: &mut SandboxConfig,
    mut resolved: sandbox::ResolvedAgentConfig,
) {
    // Merge: sandbox.yml MCPs override registry MCPs
    for (name, mcp) in &config.mcp_servers {
        resolved.mcp_servers.insert(name.clone(), mcp.clone());
    }
    config.mcp_servers = resolved.mcp_servers.clone();
    resolved.auto_mode = config.auto_mode;
    resolved.permissions = config.permissions;
    resolved.agent_type = config.agent_type;
    resolved.claude_settings = config.claude_settings.clone();
    config.resolved_agent = Some(resolved);
}

fn load_agents_registry() -> Option<sandbox::AgentsRegistryClient> {
    use sandbox::AgentsRegistryClient;

    // 1. Env var override
    if let Ok(path) = std::env::var("NANOSB_REGISTRY_PATH") {
        let p = std::path::Path::new(&path);
        if p.join("index.json").exists() {
            match AgentsRegistryClient::from_path(p) {
                Ok(client) => return Some(client),
                Err(e) => eprintln!("Warning: failed to load registry from {}: {}", path, e),
            }
        }
    }

    // 2. ~/.nanosandbox/agents-registry/
    if let Some(home) = dirs::home_dir() {
        let p = home.join(".nanosandbox").join("agents-registry");
        if p.join("index.json").exists() {
            if let Ok(client) = AgentsRegistryClient::from_path(&p) {
                return Some(client);
            }
        }
    }

    // 3. Sibling directory (development layout)
    if let Ok(cwd) = std::env::current_dir() {
        let p = cwd.join("../agents-registry");
        if p.join("index.json").exists() {
            if let Ok(client) = AgentsRegistryClient::from_path(&p) {
                return Some(client);
            }
        }
    }

    Some(AgentsRegistryClient::online_only())
}

// ========== Skills Handlers ==========

fn push_skills_feedback(app: &mut App, content: String, persistent: bool) {
    let msg = ChatMessage {
        role: MessageRole::System,
        content,
    };
    if persistent {
        app.set_system_message_persistent(msg);
    } else {
        app.set_system_message(msg);
    }
}

/// Handle `/skills list` — show skills from sandbox.yml config.
async fn handle_skills_list(app: &mut App) {
    let panel = match app.focused_panel_ref() {
        Some(p) => p,
        None => {
            push_skills_feedback(app, "No panel focused.".to_string(), true);
            return;
        }
    };

    let config = match panel.original_config.as_ref() {
        Some(c) => c,
        None => {
            push_skills_feedback(app, "No sandbox config available.".to_string(), true);
            return;
        }
    };

    let skills = &config.skills;
    let content = if skills.is_empty() {
        "No skills configured in sandbox.yml.\n\
         Add them to sandbox.yml and redeploy (nanosb apply)."
            .to_string()
    } else {
        let mut lines = vec!["Skills (from sandbox.yml):".to_string()];
        let mut sorted: Vec<_> = skills.iter().collect();
        sorted.sort();
        for name in sorted {
            lines.push(format!("  {}", name));
        }
        lines.push(String::new());
        lines.push("Changes require redeploy: edit sandbox.yml and run nanosb apply.".to_string());
        lines.join("\n")
    };
    push_skills_feedback(app, content, true);
}

/// Handle `/skills show <name>` — show skill content from the registry.
fn handle_skills_show(app: &mut App, name: &str) {
    app.set_status_message(format!("Showing skill '{}'.", name));
    let content = match &app.registry {
        Some(registry) => match registry.resolve_skill(name) {
            Ok(skill) => {
                let mut lines = Vec::new();
                lines.push(format!("Skill: {}", skill.name));
                if !skill.description.is_empty() {
                    lines.push(format!("Description: {}", skill.description));
                }
                if !skill.version.is_empty() {
                    lines.push(format!("Version: {}", skill.version));
                }
                if !skill.tags.is_empty() {
                    lines.push(format!("Tags: {}", skill.tags.join(", ")));
                }
                lines.push(String::new());
                lines.push(skill.content);
                lines.join("\n")
            }
            Err(e) => format!("Skill '{}' not found: {}", name, e),
        },
        None => "No agents registry loaded. Set NANOSB_REGISTRY_PATH or place registry at ~/.nanosandbox/agents-registry/.".to_string(),
    };

    app.set_system_message_persistent(ChatMessage {
        role: MessageRole::System,
        content,
    });
}

// ========== Agent Handlers ==========

/// Handle `/agent` — show current agent info and usage hints.
async fn handle_agent_show(app: &mut App) {
    let panel = match app.focused_panel_mut() {
        Some(p) => p,
        None => return,
    };

    let mut lines = vec![
        "Agent commands:".to_string(),
        "  /agent list        - list available agents".to_string(),
        "  /agent show <name> - show agent details".to_string(),
        "".to_string(),
        "Note: Agent config is declarative in sandbox.yml and only changes via redeploy (nanosb apply).".to_string(),
    ];

    // Show current sandbox config agent if set
    if let Some(sb) = panel.sandbox() {
        let sb = sb.lock().await;
        if let Some(resolved) = sb.config().resolved_agent.as_ref() {
            let summary = if resolved.agent_name.is_empty() && !resolved.skills.is_empty() {
                format!(
                    "Skills from sandbox config (no registry agent): {} skills, {} MCPs",
                    resolved.skills.len(),
                    resolved.mcp_servers.len(),
                )
            } else {
                format!(
                    "Current agent: {} ({} skills, {} MCPs)",
                    resolved.agent_name,
                    resolved.skills.len(),
                    resolved.mcp_servers.len(),
                )
            };
            lines.insert(0, summary);
        }
    }

    panel.chat_history.push(ChatMessage {
        role: MessageRole::System,
        content: lines.join("\n"),
    });
}

/// Handle `/agent list` — list agents from the registry (no sandbox needed).
fn handle_agent_list(app: &mut App) {
    let content = match &app.registry {
        Some(registry) => {
            let agents = registry.list_agents();
            if agents.is_empty() {
                "No agents in registry.".to_string()
            } else {
                let mut lines = Vec::new();
                lines.push("Available agents:".to_string());
                for entry in &agents {
                    let desc = if entry.description.is_empty() {
                        String::new()
                    } else {
                        format!(" - {}", entry.description)
                    };
                    let tags = if entry.tags.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", entry.tags.join(", "))
                    };
                    lines.push(format!("  {}{}{}", entry.name, desc, tags));
                }
                lines.join("\n")
            }
        }
        None => "No agents registry loaded. Set NANOSB_REGISTRY_PATH or place registry at ~/.nanosandbox/agents-registry/.".to_string(),
    };

    app.set_system_message(ChatMessage {
        role: MessageRole::System,
        content,
    });
}

/// Handle `/agent show <name>` — show agent details from the registry.
fn handle_agent_info(app: &mut App, name: &str) {
    let content = match &app.registry {
        Some(registry) => match registry.resolve_agent(name) {
            Ok(agent) => {
                let mut lines = Vec::new();
                lines.push(format!("Agent: {}", agent.name));
                if !agent.description.is_empty() {
                    lines.push(format!("Description: {}", agent.description));
                }
                if !agent.tags.is_empty() {
                    lines.push(format!("Tags: {}", agent.tags.join(", ")));
                }
                if !agent.skills.is_empty() {
                    lines.push(format!("Skills: {}", agent.skills.join(", ")));
                }
                if !agent.mcps.is_empty() {
                    let mcp_names: Vec<&str> = agent.mcps.iter().map(|m| m.name.as_str()).collect();
                    lines.push(format!("MCPs: {}", mcp_names.join(", ")));
                }
                if !agent.prompt.is_empty() {
                    let truncated = if agent.prompt.len() > 300 {
                        format!("{}...", &agent.prompt[..300])
                    } else {
                        agent.prompt.clone()
                    };
                    lines.push(format!("\nPrompt:\n{}", truncated));
                }
                lines.join("\n")
            }
            Err(e) => format!("Agent '{}' not found: {}", name, e),
        },
        None => "No agents registry loaded. Set NANOSB_REGISTRY_PATH or place registry at ~/.nanosandbox/agents-registry/.".to_string(),
    };

    app.set_system_message(ChatMessage {
        role: MessageRole::System,
        content,
    });
}

/// Encrypt and send ALL env vars to the gateway.
///
/// This is called for EVERY sandbox after start(). All env vars (from .env,
/// sandbox.yml env:, --env flags, auto-detected API keys, agent-specific vars)
/// are encrypted and sent to the gateway's SSH env store + secrets_store.
///
/// The gateway injects these into every SSH session and exec call via cmd.Env —
/// no shell export, no .env files inside the VM.
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_temp_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("nanosb-{}-{}", name, nonce));
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        dir
    }

    #[test]
    fn test_detect_agent_type_localhost_registry() {
        assert_eq!(
            detect_agent_type_from_image("localhost:5050/agent-claude:latest"),
            Some("claude".to_string())
        );
        assert_eq!(
            detect_agent_type_from_image("localhost:5050/agent-codex:latest"),
            Some("codex".to_string())
        );
        assert_eq!(
            detect_agent_type_from_image("localhost:5050/agent-goose:latest"),
            Some("goose".to_string())
        );
        assert_eq!(
            detect_agent_type_from_image("localhost:5050/agent-cursor:latest"),
            Some("cursor".to_string())
        );
    }

    #[test]
    fn test_detect_agent_type_ghcr() {
        assert_eq!(
            detect_agent_type_from_image("ghcr.io/nanosandboxai/agents-registry/claude:v1.0"),
            Some("claude".to_string())
        );
        assert_eq!(
            detect_agent_type_from_image("ghcr.io/nanosandboxai/agents-registry/codex:latest"),
            Some("codex".to_string())
        );
    }

    #[test]
    fn test_detect_agent_type_nanosb_prefix() {
        assert_eq!(
            detect_agent_type_from_image("nanosb-claude:latest"),
            Some("claude".to_string())
        );
        assert_eq!(
            detect_agent_type_from_image("nanosb-goose:v2"),
            Some("goose".to_string())
        );
    }

    #[test]
    fn test_detect_agent_type_bare_name() {
        assert_eq!(
            detect_agent_type_from_image("claude"),
            Some("claude".to_string())
        );
        assert_eq!(
            detect_agent_type_from_image("codex:latest"),
            Some("codex".to_string())
        );
    }

    #[test]
    fn test_detect_agent_type_unknown() {
        assert_eq!(detect_agent_type_from_image("alpine:3.19"), None);
        assert_eq!(detect_agent_type_from_image("my-custom-image:latest"), None);
        assert_eq!(detect_agent_type_from_image("ubuntu"), None);
    }

    #[test]
    fn test_detect_agent_session_id_from_claude_session_id_camel_case() {
        let clone_dir = make_temp_dir("claude-session-detect");
        let sessions_dir = clone_dir.join(".nanosb-state/.claude/sessions");
        fs::create_dir_all(&sessions_dir).expect("failed to create claude sessions dir");

        let session_file = sessions_dir.join("280.json");
        fs::write(
            &session_file,
            r#"{"sessionId":"8df83130-8dbe-4f28-8dff-52e44405e77d","kind":"interactive"}"#,
        )
        .expect("failed to write session file");

        let detected = detect_agent_session_id_from_state("claude", &clone_dir)
            .expect("claude session id detection should not fail");
        assert_eq!(
            detected,
            Some("8df83130-8dbe-4f28-8dff-52e44405e77d".to_string())
        );

        let _ = fs::remove_dir_all(&clone_dir);
    }

    #[test]
    fn test_should_resume_panel_requires_explicit_session_id() {
        assert!(!should_resume_panel(false, Some("sess-1")));
        assert!(!should_resume_panel(true, None));
        assert!(should_resume_panel(true, Some("sess-1")));
    }

    #[test]
    fn test_detect_goose_session_id_from_sqlite_db() {
        let clone_dir = make_temp_dir("goose-session-db");
        let db_dir = clone_dir.join(".nanosb-state/.config/goose/data/sessions");
        fs::create_dir_all(&db_dir).expect("failed to create goose sessions dir");
        let db_path = db_dir.join("sessions.db");

        let conn = rusqlite::Connection::open(&db_path).expect("failed to open sqlite db");
        conn.execute_batch(
            "
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                name TEXT,
                created_at TEXT
            );
            INSERT INTO sessions (id, name, created_at) VALUES
                ('20260520_1', 'older', '2026-05-20T10:00:00Z'),
                ('20260520_2', 'newer', '2026-05-20T11:00:00Z');
            ",
        )
        .expect("failed to seed sessions db");

        let detected = detect_goose_session_id_from_state(&clone_dir)
            .expect("goose sqlite detection should not fail");
        assert_eq!(detected, Some("20260520_2".to_string()));

        let _ = fs::remove_dir_all(&clone_dir);
    }

    #[test]
    fn test_detect_goose_session_id_no_db_returns_none() {
        let clone_dir = make_temp_dir("goose-no-db");
        let detected =
            detect_goose_session_id_from_state(&clone_dir).expect("missing db should not fail");
        assert_eq!(detected, None);
        let _ = fs::remove_dir_all(&clone_dir);
    }

    #[test]
    fn test_detect_goose_session_id_schema_error_fails() {
        let clone_dir = make_temp_dir("goose-bad-schema");
        let db_dir = clone_dir.join(".nanosb-state/.config/goose/data/sessions");
        fs::create_dir_all(&db_dir).expect("failed to create goose sessions dir");
        let db_path = db_dir.join("sessions.db");

        let conn = rusqlite::Connection::open(&db_path).expect("failed to open sqlite db");
        conn.execute_batch("CREATE TABLE not_sessions (id TEXT PRIMARY KEY);")
            .expect("failed to create incompatible schema");

        let detected = detect_goose_session_id_from_state(&clone_dir);
        assert!(detected.is_err(), "schema mismatch must fail");

        let _ = fs::remove_dir_all(&clone_dir);
    }
}
