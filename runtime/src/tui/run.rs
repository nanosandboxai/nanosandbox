//! Main event loop for the TUI.

use std::collections::HashMap;
use std::io::{self, IsTerminal};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::time::Duration;

use ratatui::crossterm::cursor::SetCursorStyle;
use ratatui::crossterm::event::{
    Event as CrosstermEvent, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
    EnableMouseCapture, DisableMouseCapture,
    EnableBracketedPaste, DisableBracketedPaste,
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

use super::app::{AgentPanel, App, ChatMessage, InputFocus, MessageRole, MouseSelection, PanelMode, SidebarFilesTab, SubmitResult};
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

    // Check for an existing session before entering the alternate screen.
    // This prompt is shown in the normal terminal (not the TUI) and must
    // happen BEFORE stderr is redirected (prompt_resume uses eprintln).
    let resume_session_data = if let Some(ref pp) = project_path {
        if let Some(session) = crate::session::Session::load(pp) {
            let issues = session.validate();
            let choice = crate::session::prompt_resume(&session, &issues);
            match choice {
                crate::session::ResumeChoice::Resume => Some(session),
                crate::session::ResumeChoice::Fresh | crate::session::ResumeChoice::Destroy => {
                    // Remove old project clones.
                    for panel in &session.panels {
                        if let Some(ref clone_path) = panel.clone_path {
                            if clone_path.exists() {
                                let _ = std::fs::remove_dir_all(clone_path);
                            }
                        }
                    }
                    // Remove session file + agent state directories.
                    let _ = crate::session::Session::delete(pp, true);
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

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
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste, SetCursorStyle::SteadyBar)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Install a panic hook that restores the terminal before printing
    // the panic message. Without this, panics corrupt the alternate screen.
    let original_hook = std::panic::take_hook();
    let saved_stderr_for_hook = saved_stderr;
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), SetCursorStyle::DefaultUserShape, DisableBracketedPaste, DisableMouseCapture, LeaveAlternateScreen);
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

    // Create shared image manager so all sandboxes coordinate pulls
    // (prevents concurrent downloads of the same image layers).
    app.image_manager = crate::image::ImageManager::with_default_cache()
        .ok()
        .map(Arc::new);

    // Try to load agents registry from well-known locations.
    app.registry = load_agents_registry();

    // Create the event channel.
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

    // Spawn terminal event reader.
    spawn_terminal_event_reader(tx.clone());

    // Resolve agent definitions + skills from registry before launching.
    let mut resolved_configs: Vec<(String, SandboxConfig)> = Vec::new();
    for (key, mut config) in sandbox_configs {
        if let (Some(ref registry), Some(ref agent_name)) = (&app.registry, &config.agent) {
            match registry.resolve_full(agent_name, &config.skills) {
                Ok(mut resolved) => {
                    // Merge: sandbox.yml MCPs override registry MCPs
                    for (name, mcp) in &config.mcp_servers {
                        resolved.mcp_servers.insert(name.clone(), mcp.clone());
                    }
                    // Replace config MCPs with merged set
                    config.mcp_servers = resolved.mcp_servers.clone();
                    // Propagate auto_mode, permissions, and agent_type from sandbox config
                    resolved.auto_mode = config.auto_mode;
                    resolved.permissions = config.permissions;
                    resolved.agent_type = config.agent_type;
                    config.resolved_agent = Some(resolved);
                }
                Err(e) => {
                    eprintln!("Warning: failed to resolve agent '{}': {}", agent_name, e);
                }
            }
        }
        resolved_configs.push((key, config));
    }

    // Either resume a previous session or start fresh sandboxes.
    if let Some(ref session) = resume_session_data {
        resume_session(&mut app, session, &tx);
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
    while let Some(event) = rx.recv().await {
        match event {
            AppEvent::Terminal(crossterm_event) => {
                match crossterm_event {
                    CrosstermEvent::Key(key) => {
                        // Clear mouse selection on any keypress.
                        app.mouse_selection = None;
                        handle_key_event(&mut app, key, &tx).await;
                    }
                    CrosstermEvent::Mouse(mouse) => {
                        handle_mouse_event(&mut app, mouse);
                    }
                    CrosstermEvent::Paste(text) => {
                        handle_paste_event(&mut app, text, &tx);
                    }
                    CrosstermEvent::Resize(_cols, _rows) => {
                        // ratatui picks up new size on next draw();
                        // render_panel() detects the delta and propagates
                        // to vt100 parser + SSH PTY.
                    }
                    CrosstermEvent::FocusGained | CrosstermEvent::FocusLost => {}
                }
            }
            AppEvent::SandboxCreating { panel_idx, .. } => {
                // Loading animation replaces status messages; nothing to do.
                let _ = panel_idx;
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
                    // Only set project_mount from sandbox if the panel doesn't
                    // already have one (resumed sessions set it up front).
                    if project_mount.is_some() {
                        panel.project_mount = project_mount;
                    }
                    // Store SSH info for later port forwarding (ssh -L).
                    if let Some((port, ref key)) = ssh_info {
                        panel.ssh_host_port = Some(port);
                        panel.ssh_key_path = Some(key.clone());
                    }
                }
                // Initiate SSH connection if SSH info is available
                if let Some((ssh_port, key_path)) = ssh_info {
                    // Pre-calculate panel dimensions for accurate PTY allocation.
                    let (pty_cols, pty_rows) = {
                        let term_size = ratatui::crossterm::terminal::size()
                            .unwrap_or((160, 40));
                        let has_sidebar = app.show_mcp_sidebar || app.show_sandbox_sidebar;
                        super::grid::estimate_panel_inner_size(
                            term_size.0,
                            term_size.1,
                            app.visible_panel_count(),
                            has_sidebar,
                            app.zoomed,
                        )
                    };
                    if let Some(panel) = app.panels.get(panel_idx) {
                        let agent_name = panel.agent_name.clone();
                        let env = panel.env.clone();
                        let workdir = panel.project_mount.as_ref().map(|_| "/workspace".to_string());
                        let permissions = panel.permissions;
                        let auto_mode = panel.auto_mode;
                        let prompt = panel.headless_state.as_ref().map(|h| h.task.clone());
                        let is_resumed = panel.is_resumed;
                        let model = panel.model.clone();
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            // Small delay for sshd to be fully ready
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            match super::terminal::connect_ssh(
                                ssh_port, key_path, pty_cols, pty_rows,
                                &agent_name, &env, workdir.as_deref(),
                                permissions, auto_mode, prompt.as_deref(),
                                is_resumed, model.as_deref(), panel_idx, tx.clone(),
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
                    panel.loading_error = Some(error);
                }
            }
            AppEvent::SshConnected { panel_idx, handle } => {
                if let Some(panel) = app.panels.get_mut(panel_idx) {
                    let (cols, rows) = panel.last_terminal_size;
                    panel.terminal = Some(super::terminal::SshTerminal::new(cols, rows));
                    panel.terminal_handle = Some(handle);
                    panel.mode = if panel.auto_mode {
                        PanelMode::Headless
                    } else {
                        PanelMode::Terminal
                    };
                    panel.reconnecting = false;
                    panel.loading_error = None;
                }
            }
            AppEvent::TerminalData { panel_idx, data } => {
                // Clear selection if terminal content changes in the selected panel
                // (but not while user is actively dragging).
                if app.mouse_selection.as_ref().is_some_and(|s| s.panel_idx == panel_idx && !s.dragging) {
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
                        }
                    } else if let Some(ref mut term) = panel.terminal {
                        term.process_bytes(&data);

                        // Extract URLs from the parsed vt100 screen, then
                        // validate the hostname.  TUI apps (ink.js) re-render
                        // the screen, sometimes garbling text — host validation
                        // rejects those broken URLs.  The first clean read is
                        // buffered for 2s (keeping the longest per host+path),
                        // then opened once.
                        let urls =
                            super::terminal::extract_urls_from_screen(term.screen());
                        let now = std::time::Instant::now();
                        for url in urls {
                            if !super::terminal::is_auth_url(&url) {
                                continue;
                            }
                            // OAuth URLs always have query parameters (?client_id=...).
                            // Truncated URLs from partial screen renders won't have
                            // reached the '?' yet — skip them.
                            if !url.contains('?') {
                                continue;
                            }
                            let key = super::terminal::url_dedup_key(&url);
                            if panel.opened_urls.contains(&key) {
                                continue;
                            }
                            // Keep the longest valid URL seen for each dedup key.
                            // Reset the debounce timer when the URL grows so we
                            // wait for the screen to stabilise after a re-render.
                            panel.pending_urls
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
                let is_reconnecting = app.panels.get(panel_idx)
                    .is_some_and(|p| p.reconnecting);
                let is_headless = app.panels.get(panel_idx)
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
                                    hs.status = "error".to_string();
                                } else {
                                    hs.status = "completed".to_string();
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
                    if let Some((name, sandbox_arc)) = kill_panel_at(&mut app, panel_idx) {
                        if let Some(sb) = sandbox_arc {
                            spawn_sandbox_destroy(sb);
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
                            // Forward OAuth callback port via SSH local-port-forward.
                            // The agent's callback server listens on 127.0.0.1 inside
                            // the VM. gvproxy expose_port sends traffic to the VM's
                            // network interface (192.168.127.2), which the server
                            // doesn't bind to. SSH -L tunnels to guest localhost.
                            if let Some(port) =
                                super::terminal::extract_oauth_callback_port(&url)
                            {
                                if !panel.forwarded_ports.contains(&port) {
                                    if let (Some(ssh_port), Some(ref key_path)) =
                                        (panel.ssh_host_port, &panel.ssh_key_path)
                                    {
                                        let fwd = format!(
                                            "{}:127.0.0.1:{}",
                                            port, port
                                        );
                                        if let Ok(child) =
                                            std::process::Command::new("ssh")
                                                .args([
                                                    "-L", &fwd,
                                                    "-p", &ssh_port.to_string(),
                                                    "-i",
                                                    &key_path.to_string_lossy(),
                                                    "-o", "StrictHostKeyChecking=no",
                                                    "-o", "UserKnownHostsFile=/dev/null",
                                                    "-o", "LogLevel=ERROR",
                                                    "-N",
                                                    "root@127.0.0.1",
                                                ])
                                                .stdin(std::process::Stdio::null())
                                                .stdout(std::process::Stdio::null())
                                                .stderr(std::process::Stdio::null())
                                                .spawn()
                                        {
                                            panel.forwarded_ports.insert(port);
                                            panel.port_forward_children.push(child);
                                        }
                                    }
                                }
                            }
                            panel.opened_urls.insert(key);
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
                let _ = execute!(terminal.backend_mut(), SetCursorStyle::DefaultUserShape, DisableMouseCapture, LeaveAlternateScreen);

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
                let _ = execute!(terminal.backend_mut(), EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste, SetCursorStyle::SteadyBar);
                terminal.clear()?;
            }
            AppEvent::UploadStarted { panel_idx, filename } => {
                let msg = format!("Uploading {}...", filename);
                // Show immediately; stays until replaced by Complete/Failed.
                // 120 ticks × 250ms = 30s (generous timeout, replaced on completion).
                if let Some(panel) = app.panels.get_mut(panel_idx) {
                    panel.notification = Some((msg, false, 120));
                }
            }
            AppEvent::UploadComplete { panel_idx, filename, remote_path, size } => {
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
            break;
        }

        // Re-render after every event.
        terminal.draw(|frame| renderer::render(frame, &mut app))?;
    }

    // Restore terminal before cleanup so the user sees progress messages.
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), SetCursorStyle::DefaultUserShape, DisableBracketedPaste, DisableMouseCapture, LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    // Restore stderr so cleanup log messages are visible.
    if saved_stderr >= 0 {
        unsafe {
            libc::dup2(saved_stderr, libc::STDERR_FILENO);
            libc::close(saved_stderr);
        }
    }

    // Determine whether to suspend (preserve session) or fully teardown.
    // Suspend when: project is mounted AND /quit (not /destroy).
    let should_suspend = app.project_path.is_some() && !app.destroy_on_quit;

    // Kill SSH port-forward processes in all cases.
    for panel in &mut app.panels {
        for child in &mut panel.port_forward_children {
            let _ = child.kill();
        }
    }

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
            let sandbox_yml_content = crate::config::file::find_sandbox_file(project_path)
                .and_then(|p| std::fs::read_to_string(p).ok())
                .unwrap_or_default();

            let session = crate::session::Session::from_app(&app, project_path, &sandbox_yml_content);
            if let Err(e) = session.save() {
                eprintln!("Warning: failed to save session: {}", e);
            } else {
                eprintln!("Session saved. Run nanosb again to resume.");
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
                let _ = crate::session::Session::delete(project_path, true);
                eprintln!("Session destroyed.");
            }
        }
    }

    // Stop/destroy all sandbox VMs.
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

/// Kill a panel's sandbox, teardown its project mount, and remove it from the panel list.
/// Returns the agent name and optional sandbox Arc for background destruction.
fn kill_panel_at(app: &mut App, idx: usize) -> Option<(String, Option<Arc<Mutex<Sandbox>>>)> {
    if idx >= app.panels.len() {
        return None;
    }

    // Teardown project mount.
    if let Some(mut pm) = app.panels[idx].project_mount.take() {
        let _ = pm.teardown();
    }

    // Kill SSH port-forward processes.
    for child in &mut app.panels[idx].port_forward_children {
        let _ = child.kill();
    }

    let sandbox_arc = app.panels[idx].sandbox.take();
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

    Some((agent_name, sandbox_arc))
}

/// Spawn a background task to destroy a sandbox.
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

/// Handle a single key event.
async fn handle_key_event(
    app: &mut App,
    key: KeyEvent,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
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
                    KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL)
                        || key.modifiers.contains(KeyModifiers::SUPER) => {
                        if let Some((panel_idx, ssh_port, key_path)) = panel_ssh_info(app) {
                            // Clone the write channel so the async task can forward
                            // Ctrl+V to the terminal if no clipboard image is found.
                            let write_tx = app.panels.get(app.focused_panel)
                                .and_then(|p| p.terminal_handle.as_ref())
                                .map(|h| h.write_tx.clone());
                            let tx = tx.clone();
                            tokio::spawn(async move {
                                let result = tokio::task::spawn_blocking(
                                    super::upload::read_clipboard_image,
                                )
                                .await;
                                match result {
                                    Ok(Ok((png_bytes, filename))) => {
                                        super::upload::spawn_bytes_upload(
                                            ssh_port, key_path, png_bytes, filename,
                                            panel_idx, tx,
                                        );
                                    }
                                    Ok(Err(_)) | Err(_) => {
                                        // No image in clipboard — forward Ctrl+V
                                        // as a regular keystroke to the terminal.
                                        if let Some(wtx) = write_tx {
                                            let _ = wtx.send(vec![0x16]); // Ctrl+V = 0x16
                                        }
                                    }
                                }
                            });
                        }
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

        // Esc: dismiss popup → dismiss autocomplete → return to global bar → close sidebar.
        KeyCode::Esc => {
            if !app.system_messages.is_empty() && !app.panels.is_empty() {
                app.system_messages.clear();
            } else if app.autocomplete_index.is_some() {
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

            let result = app.handle_submit();
            match result {
                SubmitResult::Command(cmd) => {
                    handle_command(app, cmd, tx).await;
                }
                SubmitResult::CommandError(msg) => {
                    app.set_system_message(ChatMessage {
                        role: MessageRole::System,
                        content: msg,
                    });
                }
                SubmitResult::Message(_msg) => {
                    // Panels are terminal-only; messages are not sent.
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
                }
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
            app.set_system_message(ChatMessage {
                role: MessageRole::System,
                content: concat!(
                    "Available commands:\n",
                    "  /add <agent> [--image <img>] [--project <path>] [--branch <name>] [--name <name>]\n",
                    "                                Add a new agent panel\n",
                    "  /sandboxes                    Toggle sandbox sidebar\n",
                    "  /focus <n>                    Focus panel n (0-indexed)\n",
                    "  /close [n|name]               Hide panel (sandbox keeps running)\n",
                    "  /open [n|name]                Show a hidden panel\n",
                    "  /kill [n|name]                Kill sandbox & remove panel\n",
                    "  /copy                         Copy panel content to clipboard\n",
                    "  /upload <path>                Upload host file to sandbox\n",
                    "  /paste-image                  Paste clipboard image to sandbox\n",
                    "  /zoom                         Toggle panel zoom (Ctrl+F)\n",
                    "  /clear                        Clear chat history\n",
                    "  /theme [name]                 Switch colour theme\n",
                    "  /env [KEY=VALUE]              Set/list panel env vars\n",
                    "  /reconnect                    Reconnect SSH terminal\n",
                    "  /branches                     List nanosb branches in project\n",
                    "  /mcp                          Toggle MCP sidebar\n",
                    "  /mcp list                     List MCP servers\n",
                    "  /mcp add <name> <cmd> [args]  Add MCP server\n",
                    "  /mcp remove <name>            Remove MCP server\n",
                    "  /mcp enable <name>            Enable MCP server\n",
                    "  /mcp disable <name>           Disable MCP server\n",
                    "  /skills [list]                List active skills\n",
                    "  /skills add <name>            Add skill from registry\n",
                    "  /skills remove <name>         Remove a skill\n",
                    "  /skills show <name>           Show skill details\n",
                    "  /agent                        Show current agent definition\n",
                    "  /agent set <name>             Set agent from registry\n",
                    "  /agent list                   List available agents\n",
                    "  /agent show <name>            Show agent details\n",
                    "  /gitsync [on|off|now]         Sync sandbox commits to local repo\n",
                    "  /edit [tool]                  Open clone in external tool\n",
                    "  /quit                         Suspend session and exit\n",
                    "  /destroy                      Full cleanup and exit\n",
                    "\n",
                    "  Press Esc to dismiss.\n",
                )
                .to_string(),
            });
        }
        Command::Clear => {
            if let Some(panel) = app.focused_panel_mut() {
                panel.chat_history.clear();
            } else {
                app.system_messages.clear();
            }
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
            let name = app.panels[idx].display_name.as_deref()
                .unwrap_or(&app.panels[idx].agent_name).to_string();
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
                    app.panels.iter().enumerate().rev()
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
                        let name = app.panels[i].display_name.as_deref()
                            .unwrap_or(&app.panels[i].agent_name).to_string();
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
        Command::AddAgent { agent, image, project, branch, name, auto_mode, prompt, model } => {
            add_agent(app, &agent, image.as_deref(), project.as_deref(), branch.as_deref(), name.as_deref(), auto_mode, prompt.as_deref(), model.as_deref(), tx);
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
                let term_size = ratatui::crossterm::terminal::size()
                    .unwrap_or((160, 40));
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
                // Drop existing SSH connection and reset URL tracking.
                panel.terminal = None;
                panel.terminal_handle = None;
                panel.opened_urls.clear();
                panel.pending_urls.clear();
                panel.reconnecting = true;
                panel.mode = PanelMode::Loading;
                panel.loading_error = None;
                panel.loading_tick = 0;
                // Kill SSH port-forward processes and allow re-forwarding.
                for child in &mut panel.port_forward_children {
                    let _ = child.kill();
                }
                panel.port_forward_children.clear();
                panel.forwarded_ports.clear();

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
                    let permissions = panel.permissions;
                    let auto_mode = panel.auto_mode;
                    let prompt = panel.headless_state.as_ref().map(|h| h.task.clone());
                    let is_resumed = panel.is_resumed;
                    let model = panel.model.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        match super::terminal::connect_ssh(
                            ssh_port, key_path, pty_cols, pty_rows,
                            &agent_name, &env, workdir.as_deref(),
                            permissions, auto_mode, prompt.as_deref(),
                            is_resumed, model.as_deref(), panel_idx, tx.clone(),
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
                    panel.loading_error = Some("No sandbox running. Cannot reconnect SSH.".to_string());
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

            if let Some((agent_name, sandbox_arc)) = kill_panel_at(app, idx) {
                if let Some(sb) = sandbox_arc {
                    spawn_sandbox_destroy(sb);
                }

                app.set_system_message(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Killed '{}'.", agent_name),
                });
            }
        }
        Command::McpList
        | Command::McpAdd { .. }
        | Command::McpRemove { .. }
        | Command::McpEnable { .. }
        | Command::McpDisable { .. } => {
            if app.panels.is_empty() {
                app.set_system_message(ChatMessage {
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
                app.set_system_message(chat_msg);
            } else {
                app.set_system_message(ChatMessage {
                    role: MessageRole::System,
                    content: "No project configured. Use --project flag when launching nanosb.".to_string(),
                });
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
        Command::Edit { tool } => {
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
        Command::SkillsList
        | Command::SkillsAdd { .. }
        | Command::SkillsRemove { .. }
        | Command::SkillsShow { .. } => {
            if app.panels.is_empty() {
                app.set_system_message(ChatMessage {
                    role: MessageRole::System,
                    content: "Skills commands require an active panel. Use /add <agent> first."
                        .to_string(),
                });
            } else {
                match cmd {
                    Command::SkillsList => handle_skills_list(app).await,
                    Command::SkillsAdd { name } => handle_skills_add(app, &name).await,
                    Command::SkillsRemove { name } => handle_skills_remove(app, &name).await,
                    Command::SkillsShow { name } => handle_skills_show(app, &name),
                    _ => unreachable!(),
                }
            }
        }

        // ===== Agent commands =====
        Command::AgentShow | Command::AgentSet { .. } => {
            if app.panels.is_empty() {
                app.set_system_message(ChatMessage {
                    role: MessageRole::System,
                    content: "Agent commands require an active panel. Use /add <agent> first."
                        .to_string(),
                });
            } else {
                match cmd {
                    Command::AgentShow => handle_agent_show(app).await,
                    Command::AgentSet { name } => handle_agent_set(app, &name).await,
                    _ => unreachable!(),
                }
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

/// Get the SSH port and key path from the focused panel, if available.
fn panel_ssh_info(app: &App) -> Option<(usize, u16, std::path::PathBuf)> {
    let idx = app.focused_panel;
    let panel = app.panels.get(idx)?;
    let port = panel.ssh_host_port?;
    let key = panel.ssh_key_path.clone()?;
    Some((idx, port, key))
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
    let (panel_idx, ssh_port, key_path) = match panel_ssh_info(app) {
        Some(info) => info,
        None => {
            app.set_system_message(ChatMessage {
                role: MessageRole::System,
                content: "No active SSH session. Wait for sandbox to be ready.".to_string(),
            });
            return;
        }
    };

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

    super::upload::spawn_file_upload(ssh_port, key_path, host_path, panel_idx, tx.clone());
}

/// Handle the `/paste-image` command.
fn handle_paste_image(app: &mut App, tx: &mpsc::UnboundedSender<AppEvent>) {
    let (panel_idx, ssh_port, key_path) = match panel_ssh_info(app) {
        Some(info) => info,
        None => {
            app.set_system_message(ChatMessage {
                role: MessageRole::System,
                content: "No active SSH session. Wait for sandbox to be ready.".to_string(),
            });
            return;
        }
    };

    app.set_status_message("Reading clipboard image...");
    let tx = tx.clone();

    tokio::spawn(async move {
        // Clipboard access is blocking — run in spawn_blocking.
        let result = tokio::task::spawn_blocking(super::upload::read_clipboard_image).await;

        match result {
            Ok(Ok((png_bytes, filename))) => {
                super::upload::spawn_bytes_upload(
                    ssh_port, key_path, png_bytes, filename, panel_idx, tx,
                );
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
fn handle_paste_event(
    app: &mut App,
    text: String,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    // If focus is on the global input bar, insert the text there.
    if app.input_focus == InputFocus::Global {
        app.global_input.insert_str(&text);
        return;
    }

    // In Terminal mode: if the paste is empty (image-only clipboard via Cmd+V),
    // check the clipboard for an image to upload.
    if text.is_empty() {
        if let Some((panel_idx, ssh_port, key_path)) = panel_ssh_info(app) {
            let tx = tx.clone();
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(
                    super::upload::read_clipboard_image,
                )
                .await;
                match result {
                    Ok(Ok((png_bytes, filename))) => {
                        super::upload::spawn_bytes_upload(
                            ssh_port, key_path, png_bytes, filename,
                            panel_idx, tx,
                        );
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
        return;
    }

    // Forward pasted text as-is to the terminal (covers drag-and-drop paths too).
    if let Some(panel) = app.panels.get(app.focused_panel) {
        if panel.mode == PanelMode::Terminal {
            if let Some(ref handle) = panel.terminal_handle {
                let _ = handle.write_tx.send(text.into_bytes());
            }
        }
    }
}

/// Handle a mouse event for panel-scoped text selection.
fn handle_mouse_event(app: &mut App, mouse: MouseEvent) {
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
                if app.panels.get(panel_idx).is_some_and(|p| {
                    p.mode == PanelMode::Terminal && p.terminal.is_some()
                }) {
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
                        let text =
                            term.screen().contents_between(start.0, start.1, end.0, end.1);
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
        if x >= area.x
            && x < area.x + area.width
            && y >= area.y
            && y < area.y + area.height
        {
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
/// - `ghcr.io/devdone-labs/agents-registry/codex:v1` → `"codex"`
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

        // Try to parse as JSON
        let json: serde_json::Value = match serde_json::from_str(clean) {
            Ok(v) => v,
            Err(_) => continue, // Not JSON — shell prompt, etc.
        };

        state.raw_lines.push(clean.to_string());

        // Transition from "starting" once we receive any valid JSON.
        if state.status == "starting" {
            state.status = "running".to_string();
        }

        let event_type = json
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("");

        match event_type {
            // =================================================================
            // Claude Code: -p --output-format stream-json
            // =================================================================

            // Token-level streaming delta (requires --include-partial-messages)
            "stream_event" => {
                if let Some(delta_text) = json
                    .pointer("/event/delta/text")
                    .and_then(|v| v.as_str())
                {
                    state.agent_text.push_str(delta_text);
                    state.status = "thinking".to_string();
                }
                // Tool use block start
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
            // Complete assistant turn (text + tool_use blocks in content array)
            "assistant" => {
                if let Some(content) = json.get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for block in content {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            state.agent_text.push_str(text);
                            state.agent_text.push('\n');
                        }
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                            let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");
                            let input = block.get("input").map(|v| v.to_string()).unwrap_or_default();
                            state.tool_calls.push(super::app::HeadlessToolCall {
                                tool_name: name.to_string(),
                                input_summary: truncate_str(&input, 80),
                                output_preview: String::new(),
                                status: "running".to_string(),
                            });
                            state.status = "tool_use".to_string();
                        }
                    }
                }
            }
            // Claude final result
            "result" => {
                if let Some(text) = json.get("result").and_then(|r| r.as_str()) {
                    state.agent_text.push_str(text);
                }
                state.status = "completed".to_string();
            }

            // =================================================================
            // Codex: exec --json (NDJSON streaming)
            // =================================================================

            // Lifecycle events (no content to extract)
            "thread.started" | "turn.started" => {}
            "turn.completed" => {
                // Mark last tool as done if still running
                if let Some(last) = state.tool_calls.last_mut() {
                    if last.status == "running" {
                        last.status = "done".to_string();
                    }
                }
            }
            // Item events — the main content carriers
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
                                let cmd = item.get("command")
                                    .and_then(|v| v.as_str()).unwrap_or("command");
                                state.tool_calls.push(super::app::HeadlessToolCall {
                                    tool_name: "command".to_string(),
                                    input_summary: truncate_str(cmd, 80),
                                    output_preview: String::new(),
                                    status: "running".to_string(),
                                });
                                state.status = "tool_use".to_string();
                            }
                            if event_type == "item.completed" {
                                if let Some(last) = state.tool_calls.last_mut() {
                                    last.status = "done".to_string();
                                    if let Some(output) = item.get("aggregated_output")
                                        .and_then(|v| v.as_str())
                                    {
                                        last.output_preview = truncate_str(output, 120);
                                    }
                                }
                            }
                        }
                        "file_change" => {
                            if event_type == "item.started" {
                                let path = item.get("path")
                                    .and_then(|v| v.as_str()).unwrap_or("file");
                                state.tool_calls.push(super::app::HeadlessToolCall {
                                    tool_name: "file_change".to_string(),
                                    input_summary: truncate_str(path, 80),
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
                        "mcp_tool_call" | "web_search" => {
                            if event_type == "item.started" {
                                let name = item.get("tool")
                                    .or_else(|| item.get("type"))
                                    .and_then(|v| v.as_str()).unwrap_or("tool");
                                let input = item.get("arguments")
                                    .or_else(|| item.get("query"))
                                    .map(|v| v.to_string()).unwrap_or_default();
                                state.tool_calls.push(super::app::HeadlessToolCall {
                                    tool_name: name.to_string(),
                                    input_summary: truncate_str(&input, 80),
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
                        _ => {}
                    }
                }
            }

            // =================================================================
            // Goose: run --output-format stream-json
            // =================================================================

            // Message event — assistant text and tool requests
            "message" => {
                if let Some(content) = json.get("message")
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
                                let name = block.pointer("/tool_call/name")
                                    .and_then(|v| v.as_str()).unwrap_or("tool");
                                let args = block.pointer("/tool_call/arguments")
                                    .map(|v| v.to_string()).unwrap_or_default();
                                state.tool_calls.push(super::app::HeadlessToolCall {
                                    tool_name: name.to_string(),
                                    input_summary: truncate_str(&args, 80),
                                    output_preview: String::new(),
                                    status: "running".to_string(),
                                });
                                state.status = "tool_use".to_string();
                            }
                            "tool_response" => {
                                // Mark matching tool call as done
                                if let Some(last) = state.tool_calls.last_mut() {
                                    if last.status == "running" {
                                        last.status = "done".to_string();
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            // Notification — extension log/progress messages
            "notification" => {
                if let Some(msg) = json.pointer("/log/message").and_then(|v| v.as_str()) {
                    let ext = json.get("extension_id")
                        .and_then(|v| v.as_str()).unwrap_or("ext");
                    state.tool_calls.push(super::app::HeadlessToolCall {
                        tool_name: ext.to_string(),
                        input_summary: truncate_str(msg, 80),
                        output_preview: String::new(),
                        status: "done".to_string(),
                    });
                }
            }
            // Goose completion
            "complete" => {
                state.status = "completed".to_string();
            }

            // =================================================================
            // Cursor: -p --output-format stream-json
            // =================================================================

            // Thinking deltas — token-level streaming of reasoning.
            // Accumulate without newlines; only add a newline on "completed".
            "thinking" => {
                let subtype = json.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
                if subtype == "delta" {
                    if let Some(text) = json.get("text").and_then(|v| v.as_str()) {
                        state.agent_text.push_str(text);
                    }
                    state.status = "thinking".to_string();
                } else if subtype == "completed" {
                    // Add a newline after the full thinking block.
                    if !state.agent_text.is_empty() && !state.agent_text.ends_with('\n') {
                        state.agent_text.push('\n');
                    }
                }
            }

            // Tool call lifecycle — tool name is the KEY inside the `tool_call` object.
            // e.g. {"tool_call": {"shellToolCall": {"args": {"command": "ls"}}}}
            "tool_call" => {
                let subtype = json.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
                if subtype == "started" {
                    // Extract tool name from the first key of the tool_call object.
                    let tc_obj = json.get("tool_call").and_then(|v| v.as_object());
                    let (name, input) = if let Some(obj) = tc_obj {
                        let key = obj.keys().next().map(|k| k.as_str()).unwrap_or("tool");
                        // Friendly name: strip "ToolCall" suffix.
                        let friendly = key.strip_suffix("ToolCall").unwrap_or(key);
                        // Extract primary arg: command, path, query, or pattern.
                        let args = obj.values().next()
                            .and_then(|v| v.get("args"))
                            .and_then(|a| a.as_object());
                        let input = args.and_then(|a| {
                            a.get("command").or_else(|| a.get("path"))
                                .or_else(|| a.get("query")).or_else(|| a.get("pattern"))
                                .and_then(|v| v.as_str())
                        }).unwrap_or("");
                        (friendly.to_string(), input.to_string())
                    } else {
                        ("tool".to_string(), String::new())
                    };
                    state.tool_calls.push(super::app::HeadlessToolCall {
                        tool_name: name,
                        input_summary: truncate_str(&input, 80),
                        output_preview: String::new(),
                        status: "running".to_string(),
                    });
                    state.status = "tool_use".to_string();
                } else if subtype == "completed" {
                    if let Some(last) = state.tool_calls.last_mut() {
                        last.status = "done".to_string();
                        // Extract output preview for shell commands.
                        if let Some(output) = json.pointer("/tool_call/shellToolCall/result/success/output")
                            .and_then(|v| v.as_str())
                        {
                            last.output_preview = truncate_str(output, 120);
                        }
                    }
                }
            }

            // =================================================================
            // Lifecycle and system events
            // =================================================================

            // System init — extract model info for display
            "system" => {
                if let Some(model) = json.get("model").and_then(|v| v.as_str()) {
                    state.status = format!("running ({})", model);
                }
            }

            // Tool result events (Claude `user` with tool_result)
            "user" => {
                // Mark the last tool call as done when we get its result
                if let Some(last) = state.tool_calls.last_mut() {
                    if last.status == "running" {
                        last.status = "done".to_string();
                    }
                }
            }

            // Errors — show as text so the user sees them
            "error" | "turn.failed" => {
                let msg = json.get("error")
                    .and_then(|e| e.get("message").or(Some(e)))
                    .and_then(|v| v.as_str())
                    .or_else(|| json.get("message").and_then(|v| v.as_str()))
                    .unwrap_or("unknown error");
                state.agent_text.push_str(&format!("[error] {}\n", msg));
                state.status = "error".to_string();
            }

            // Silent lifecycle events
            "model_change" => {}

            _ => {
                // Unknown event — try to extract text but don't add newlines
                // (could be streaming deltas from an unknown agent).
                if let Some(text) = json.get("text").and_then(|v| v.as_str()) {
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
    auto_mode: bool,
    prompt: Option<&str>,
    model: Option<&str>,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    let image_name = match image {
        Some(img) => crate::config::normalize_image(img),
        None => crate::config::normalize_image(agent),
    };

    let mut panel = AgentPanel::new(agent);

    // Headless mode setup.
    panel.auto_mode = auto_mode;
    if auto_mode {
        panel.permissions = crate::config::Permissions::AllowAll;
        let task = prompt.unwrap_or("(no prompt)");
        panel.headless_state = Some(super::app::HeadlessState::new(task));
    }

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

    // Set model selection.
    panel.model = model.map(String::from);

    // Build sandbox config.
    // Agent VMs need enough memory for the agent CLI + runtime overhead.
    let project_path = project
        .map(std::path::PathBuf::from)
        .or_else(|| app.project_path.clone());

    let mut builder = SandboxConfig::builder()
        .image(&image_name)
        .memory_mb(1024);

    // Set agent type from agent name.
    if let Ok(agent_type) = agent.parse::<crate::config::AgentType>() {
        builder = builder.agent_type(agent_type);
    }

    // Set model if provided.
    if let Some(m) = model {
        builder = builder.model(m);
    }

    if let Some(n) = name {
        builder = builder.name(n);
    }

    if let Some(ref pp) = project_path {
        builder = builder.project(pp, branch);
    }

    let mut config = builder.build();

    // Pass auto_sync setting to project config so sandbox creation
    // knows whether to use setup() or setup_deferred().
    if let Some(ref mut proj) = config.project {
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
    let image_manager = app.image_manager.clone();
    tokio::spawn(async move {
        let create_result = if let Some(im) = image_manager {
            Sandbox::create_with_manager(config, im).await
        } else {
            Sandbox::create(config).await
        };
        match create_result {
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
    mut config: SandboxConfig,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    let display_name = config.name.clone();

    // Detect the base agent type: explicit config.agent_type > image name > key.
    let agent_type = config
        .agent_type
        .map(|t| t.to_string())
        .or_else(|| detect_agent_type_from_image(&config.image))
        .unwrap_or_else(|| key.to_string());

    let mut panel = AgentPanel::new(&agent_type);
    panel.display_name = Some(display_name.clone());
    panel.auto_mode = config.auto_mode;
    panel.permissions = config.permissions;
    panel.model = config.model.clone();
    if config.auto_mode {
        let task = config.prompt.as_deref().unwrap_or("(no prompt)");
        panel.headless_state = Some(super::app::HeadlessState::new(task));
    }

    // Copy env vars from config to panel.
    for (k, v) in &config.env {
        panel.env.insert(k.clone(), v.clone());
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
    if config.project.is_none() {
        if let Some(ref project_path) = app.project_path {
            config.project = Some(crate::config::ProjectConfig {
                path: project_path.clone(),
                branch: None,
                mount_point: "/workspace".to_string(),
                auto_sync: app.settings.gitsync.auto_sync,
            });
        }
    } else {
        // Config has a project — just set auto_sync from settings.
        if let Some(ref mut proj) = config.project {
            proj.auto_sync = app.settings.gitsync.auto_sync;
        }
    }

    // Store the config for session persistence.
    panel.original_config = Some(config.clone());

    app.panels.push(panel);
    let panel_idx = app.panels.len() - 1;
    app.focused_panel = panel_idx;
    app.show_welcome = false;
    app.focus_panel_input();

    let tx = tx.clone();
    let image_manager = app.image_manager.clone();
    tokio::spawn(async move {
        let create_result = if let Some(im) = image_manager {
            Sandbox::create_with_manager(config, im).await
        } else {
            Sandbox::create(config).await
        };
        match create_result {
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


/// Resume panels from a saved session.
///
/// For each panel in the session, this creates a fresh sandbox with the saved config
/// but reuses the existing project clone (if still present). The agent is launched
/// with its resume command variant so it can pick up the previous conversation.
fn resume_session(
    app: &mut App,
    session: &crate::session::Session,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    for sp in &session.panels {
        let mut config = sp.config.clone();

        // Normalize the image in case the session was saved with a bare name.
        config.image = crate::config::normalize_image(&config.image);

        // Re-populate env vars from host environment (secrets are not stored in session).
        for key in &sp.env_keys {
            if let Ok(val) = std::env::var(key) {
                config.env.insert(key.clone(), val);
            }
        }

        // Set up the agent panel.
        let agent_type = detect_agent_type_from_image(&config.image)
            .unwrap_or_else(|| sp.agent_name.clone());

        let mut panel = AgentPanel::new(&agent_type);
        panel.display_name = sp.display_name.clone();
        panel.auto_mode = sp.auto_mode;
        panel.permissions = sp.permissions;
        if sp.auto_mode {
            let task = config.prompt.as_deref().unwrap_or("(no prompt)");
            panel.headless_state = Some(super::app::HeadlessState::new(task));
        }
        panel.visible = sp.visible;
        panel.agent_type = sp.agent_type;
        panel.model = sp.model.clone();
        panel.original_config = Some(config.clone());

        // Copy env vars from config to panel.
        for (k, v) in &config.env {
            panel.env.insert(k.clone(), v.clone());
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
        // creating a new one. We set config.project = None so that
        // setup_project_mount() inside Sandbox::create() is skipped, and add
        // the VirtioFS mount for the existing clone ourselves.
        if let Some(ref clone_path) = sp.clone_path {
            if clone_path.exists() {
                // Add VirtioFS mount for existing clone directly.
                config.mounts.push(crate::config::Mount::virtiofs(clone_path, "/workspace"));
                // Do NOT set config.project — this skips setup_project_mount().
                config.project = None;

                // Create a ProjectMount on the panel to handle suspend/teardown.
                if let Ok(mut pm) = crate::project::ProjectMount::detect(&session.project_path) {
                    let _ = pm.resume(clone_path, sp.branches.clone());
                    panel.project_mount = Some(pm);
                }
            } else {
                // Clone dir is gone — create a fresh clone from the branch.
                config.project = Some(crate::config::ProjectConfig {
                    path: session.project_path.clone(),
                    branch: sp
                        .branches
                        .first()
                        .map(|(_, b)| b.clone()),
                    mount_point: "/workspace".to_string(),
                    auto_sync: app.settings.gitsync.auto_sync,
                });
            }
        }

        // Mark panel as resumed so agent uses resume command variant.
        // Agent state is stored in /workspace/.nanosb-state/ (inside the clone),
        // so it's automatically available when the clone is re-mounted.
        panel.is_resumed = true;

        app.panels.push(panel);
        let panel_idx = app.panels.len() - 1;
        app.focused_panel = panel_idx;
        app.show_welcome = false;
        app.focus_panel_input();

        // Spawn sandbox creation in background.
        let tx = tx.clone();
        let image_manager = app.image_manager.clone();
        tokio::spawn(async move {
            let create_result = if let Some(im) = image_manager {
                Sandbox::create_with_manager(config, im).await
            } else {
                Sandbox::create(config).await
            };
            match create_result {
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

// ========== Agents Registry Loader ==========

/// Try to load the agents registry from well-known locations.
///
/// Search order:
/// 1. `NANOSB_REGISTRY_PATH` environment variable
/// 2. `~/.nanosandbox/agents-registry/`
/// 3. `../agents-registry/` (sibling directory — dev setup)
fn load_agents_registry() -> Option<crate::agents_registry::AgentsRegistryClient> {
    use crate::agents_registry::AgentsRegistryClient;

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

    None
}

// ========== Skills Handlers ==========

/// Handle `/skills list` — list skills installed in the gateway.
async fn handle_skills_list(app: &mut App) {
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
    match sb.list_skills().await {
        Ok(skills) => {
            if skills.is_empty() {
                panel.chat_history.push(ChatMessage {
                    role: MessageRole::System,
                    content: "No skills configured. Use /skills add <name> to add from the registry.".to_string(),
                });
            } else {
                let mut lines = Vec::new();
                lines.push("Skills:".to_string());
                for (name, skill) in &skills {
                    let desc = if skill.description.is_empty() {
                        String::new()
                    } else {
                        format!(" - {}", skill.description)
                    };
                    lines.push(format!("  {}{}", name, desc));
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
                content: format!("Failed to list skills: {}", e),
            });
        }
    }
}

/// Handle `/skills add <name>` — resolve from registry and push to gateway.
async fn handle_skills_add(app: &mut App, name: &str) {
    // First resolve the skill from the registry (host-side).
    let skill = match &app.registry {
        Some(registry) => match registry.resolve_skill(name) {
            Ok(s) => s,
            Err(e) => {
                if let Some(panel) = app.focused_panel_mut() {
                    panel.chat_history.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("Failed to resolve skill '{}': {}", name, e),
                    });
                }
                return;
            }
        },
        None => {
            if let Some(panel) = app.focused_panel_mut() {
                panel.chat_history.push(ChatMessage {
                    role: MessageRole::System,
                    content: "No agents registry loaded. Set NANOSB_REGISTRY_PATH or place registry at ~/.nanosandbox/agents-registry/.".to_string(),
                });
            }
            return;
        }
    };

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
    match sb.add_skill(&skill).await {
        Ok(()) => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: format!("Skill '{}' added.", name),
            });
        }
        Err(e) => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: format!("Failed to add skill '{}': {}", name, e),
            });
        }
    }
}

/// Handle `/skills remove <name>`.
async fn handle_skills_remove(app: &mut App, name: &str) {
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
    match sb.remove_skill(name).await {
        Ok(()) => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: format!("Skill '{}' removed.", name),
            });
        }
        Err(e) => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: format!("Failed to remove skill '{}': {}", name, e),
            });
        }
    }
}

/// Handle `/skills show <name>` — show skill content from the registry.
fn handle_skills_show(app: &mut App, name: &str) {
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

    if let Some(panel) = app.focused_panel_mut() {
        panel.chat_history.push(ChatMessage {
            role: MessageRole::System,
            content,
        });
    }
}

// ========== Agent Handlers ==========

/// Handle `/agent` — show current agent info and usage hints.
async fn handle_agent_show(app: &mut App) {
    let panel = match app.focused_panel_mut() {
        Some(p) => p,
        None => return,
    };

    let mut lines = vec![
        "Agent definition commands:".to_string(),
        "  /agent set <name>  - set agent definition from registry".to_string(),
        "  /agent list        - list available agents".to_string(),
        "  /agent show <name> - show agent details".to_string(),
    ];

    // Show current sandbox config agent if set
    if let Some(sb) = panel.sandbox.as_ref() {
        let sb = sb.lock().await;
        if let Some(resolved) = sb.config().resolved_agent.as_ref() {
            lines.insert(0, format!("Current agent: {} ({} skills, {} MCPs)",
                resolved.agent_name,
                resolved.skills.len(),
                resolved.mcp_servers.len(),
            ));
        }
    }

    panel.chat_history.push(ChatMessage {
        role: MessageRole::System,
        content: lines.join("\n"),
    });
}

/// Handle `/agent set <name>` — resolve from registry, bootstrap, and restart.
async fn handle_agent_set(app: &mut App, name: &str) {
    // First resolve full agent config from registry.
    let mut resolved = match &app.registry {
        Some(registry) => match registry.resolve_full(name, &[]) {
            Ok(r) => r,
            Err(e) => {
                if let Some(panel) = app.focused_panel_mut() {
                    panel.chat_history.push(ChatMessage {
                        role: MessageRole::System,
                        content: format!("Failed to resolve agent '{}': {}", name, e),
                    });
                }
                return;
            }
        },
        None => {
            if let Some(panel) = app.focused_panel_mut() {
                panel.chat_history.push(ChatMessage {
                    role: MessageRole::System,
                    content: "No agents registry loaded. Set NANOSB_REGISTRY_PATH or place registry at ~/.nanosandbox/agents-registry/.".to_string(),
                });
            }
            return;
        }
    };

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

    // Inherit auto_mode, permissions, and agent_type from sandbox config
    let sb = sandbox.lock().await;
    resolved.auto_mode = sb.config().auto_mode;
    resolved.permissions = sb.config().permissions;
    resolved.agent_type = sb.config().agent_type;
    match sb.bootstrap_agent(&resolved).await {
        Ok(()) => {
            let skill_count = resolved.skills.len();
            let mcp_count = resolved.mcp_servers.len();
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: format!(
                    "Agent '{}' configured ({} skills, {} MCPs).",
                    name, skill_count, mcp_count,
                ),
            });
        }
        Err(e) => {
            panel.chat_history.push(ChatMessage {
                role: MessageRole::System,
                content: format!("Failed to set agent '{}': {}", name, e),
            });
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
            detect_agent_type_from_image("ghcr.io/devdone-labs/agents-registry/claude:v1.0"),
            Some("claude".to_string())
        );
        assert_eq!(
            detect_agent_type_from_image("ghcr.io/devdone-labs/agents-registry/codex:latest"),
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
}
