#![cfg(test)]

//! Headless TUI test suite: frames, handlers, and synthetic event loop.
//!
//! All tests use `ratatui::backend::TestBackend` — no TTY, no VM, no real terminal.
//! Handler tests use `#[tokio::test]` for async `handle_command` / `handle_event`.

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;
use tokio::sync::mpsc;

use super::app::{AgentPanel, App, ChatMessage, MessageRole, PanelMode};
use super::commands::{self, Command, ParseResult};
use super::event::AppEvent;
use super::run::{handle_command, handle_event};
use super::terminal::SshTerminal;
use super::theme::ThemeName;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Render the current app state into a TestBackend and return the backend,
/// so tests can inspect both text and per-cell styles.
fn render_backend(app: &mut App, w: u16, h: u16) -> TestBackend {
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| super::renderer::render(f, app))
        .unwrap();
    terminal.backend().clone()
}

/// Render the current app state into a TestBackend and return the screen text.
fn render_screen(app: &mut App, w: u16, h: u16) -> String {
    screen_text(render_backend(app, w, h).buffer())
}

/// Concatenate every cell symbol row by row (respecting width).
fn screen_text(buf: &Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for row in 0..area.height {
        for col in 0..area.width {
            let cell = buf.cell((col, row)).unwrap();
            out.push_str(cell.symbol());
        }
        if row + 1 < area.height {
            out.push('\n');
        }
    }
    out
}

/// Foreground colour of a panel's top-left border corner.
///
/// `panel_areas` stores each panel's INNER area, so the border corner sits one
/// cell up-left of it (`x-1, y-1`).
fn border_fg(buf: &Buffer, inner: ratatui::layout::Rect) -> Option<ratatui::style::Color> {
    let x = inner.x.checked_sub(1)?;
    let y = inner.y.checked_sub(1)?;
    buf.cell((x, y)).map(|c| c.fg)
}

// ── T8 — Frame tests (TestBackend, no TTY) ───────────────────────────────────

#[test]
fn frame_welcome() {
    let mut app = App::new();
    let text = render_screen(&mut app, 80, 40);
    // The welcome screen shows "No agent panels are open." or "Getting Started"
    // depending on the theme. Both are valid welcome markers.
    assert!(
        text.contains("No agent panels") || text.contains("Getting Started"),
        "welcome screen should contain a welcome marker, got: {text:?}"
    );
    assert!(
        text.contains("/add"),
        "welcome screen should contain '/add', got: {text:?}"
    );
}

#[test]
fn frame_panel_loading() {
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.mode = PanelMode::Loading;
    panel.loading_message = Some("Booting microVM...".into());
    app.panels.push(panel);
    app.focused_panel = 0;
    app.show_welcome = false;

    let text = render_screen(&mut app, 80, 40);
    assert!(
        text.contains("Booting microVM"),
        "loading message should appear, got: {text:?}"
    );
}

#[test]
fn frame_panel_error() {
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.mode = PanelMode::Loading;
    panel.loading_error = Some("boom".into());
    app.panels.push(panel);
    app.focused_panel = 0;
    app.show_welcome = false;

    let text = render_screen(&mut app, 80, 40);
    assert!(
        text.contains("boom"),
        "error message should appear, got: {text:?}"
    );
}

#[test]
fn frame_panel_terminal() {
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.mode = PanelMode::Terminal;
    let mut term = SshTerminal::new(80, 24);
    term.process_bytes(b"hello-term");
    panel.terminal = Some(term);
    app.panels.push(panel);
    app.focused_panel = 0;
    app.show_welcome = false;

    let text = render_screen(&mut app, 80, 40);
    assert!(
        text.contains("hello-term"),
        "terminal content should appear, got: {text:?}"
    );
}

#[test]
fn terminal_panel_default_background_is_theme_black() {
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.mode = PanelMode::Terminal;
    let mut term = SshTerminal::new(80, 24);
    term.process_bytes(b"hello-term");
    panel.terminal = Some(term);
    app.panels.push(panel);
    app.focused_panel = 0;
    app.show_welcome = false;

    let bg = app.theme.background;
    let backend = render_backend(&mut app, 80, 40);
    let buf = backend.buffer();

    let mut first_terminal_cell = None;
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            if let Some(c) = buf.cell((x, y)) {
                assert_ne!(
                    c.bg,
                    ratatui::style::Color::Reset,
                    "cell at ({x},{y}) uses the host default bg"
                );
                if c.symbol() == "h" && first_terminal_cell.is_none() {
                    first_terminal_cell = Some(c.bg);
                }
            }
        }
    }
    assert_eq!(
        first_terminal_cell,
        Some(bg),
        "terminal default bg should be the theme background"
    );
}

#[test]
fn frame_multi_panel_grid() {
    let mut app = App::new();
    // Distinct multi-char names so a title match cannot be a substring of
    // unrelated screen text.
    for name in &["alpha-panel", "beta-panel", "gamma-panel"] {
        let mut panel = AgentPanel::new(name);
        panel.mode = PanelMode::Terminal;
        let mut term = SshTerminal::new(40, 12);
        term.process_bytes(b"panel");
        panel.terminal = Some(term);
        app.panels.push(panel);
    }
    app.focused_panel = 0;
    app.show_welcome = false;

    let text = render_screen(&mut app, 80, 40);
    assert_eq!(app.panel_areas.len(), 3, "should have 3 panel areas");
    for name in ["alpha-panel", "beta-panel", "gamma-panel"] {
        assert!(
            text.contains(name),
            "each panel title should render, missing {name:?} in: {text:?}"
        );
    }
}

#[test]
fn frame_mcp_sidebar() {
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.mode = PanelMode::Loading;
    app.panels.push(panel);
    app.focused_panel = 0;
    app.show_welcome = false;
    app.show_mcp_sidebar = true;

    let text = render_screen(&mut app, 80, 40);
    assert!(
        text.contains("MCP Servers"),
        "MCP sidebar should show 'MCP Servers', got: {text:?}"
    );
}

#[test]
fn frame_sandbox_sidebar() {
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.mode = PanelMode::Loading;
    app.panels.push(panel);
    app.focused_panel = 0;
    app.show_welcome = false;
    app.show_sandbox_sidebar = true;

    let text = render_screen(&mut app, 80, 40);
    assert!(
        text.contains("Sandboxes"),
        "sandbox sidebar should show 'Sandboxes', got: {text:?}"
    );
}

#[test]
fn frame_focus_border() {
    let mut app = App::new();
    for name in &["alpha", "beta"] {
        let mut panel = AgentPanel::new(name);
        panel.mode = PanelMode::Terminal;
        let mut term = SshTerminal::new(40, 12);
        term.process_bytes(b"content");
        panel.terminal = Some(term);
        app.panels.push(panel);
    }
    app.focused_panel = 0;
    app.show_welcome = false;

    // The focused panel's border is drawn in `accent`; unfocused in `text_muted`.
    // Render once with panel 0 focused, capture each panel's border colour.
    let backend0 = render_backend(&mut app, 80, 40);
    let buf0 = backend0.buffer();
    assert_eq!(app.panel_areas.len(), 2);
    let (idx_a0, area_a0) = app.panel_areas[0];
    let (idx_b0, area_b0) = app.panel_areas[1];
    let a0 = border_fg(buf0, area_a0);
    let b0 = border_fg(buf0, area_b0);
    assert_ne!(
        a0, b0,
        "focused panel border colour must differ from unfocused"
    );

    // Now focus panel 1: the border colours should swap.
    app.focused_panel = 1;
    let backend1 = render_backend(&mut app, 80, 40);
    let buf1 = backend1.buffer();
    let area_a1 = app
        .panel_areas
        .iter()
        .find(|(i, _)| *i == idx_a0)
        .map(|(_, r)| *r)
        .unwrap();
    let area_b1 = app
        .panel_areas
        .iter()
        .find(|(i, _)| *i == idx_b0)
        .map(|(_, r)| *r)
        .unwrap();
    let a1 = border_fg(buf1, area_a1);
    let b1 = border_fg(buf1, area_b1);
    assert_eq!(a1, b0, "panel A should now be unfocused");
    assert_eq!(b1, a0, "panel B should now be focused");
}

#[tokio::test]
async fn frame_help_overlay() {
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.mode = PanelMode::Loading;
    app.panels.push(panel);
    app.focused_panel = 0;
    app.show_welcome = false;

    let (tx, _rx) = mpsc::unbounded_channel();
    handle_command(&mut app, Command::Help, &tx).await;

    let text = render_screen(&mut app, 80, 40);
    assert!(
        text.contains("Available commands"),
        "help overlay should contain 'Available commands', got: {text:?}"
    );
}

#[test]
fn frame_status_message() {
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.mode = PanelMode::Loading;
    app.panels.push(panel);
    app.focused_panel = 0;
    app.show_welcome = false;

    app.set_status_message("saved!");

    let text = render_screen(&mut app, 80, 40);
    assert!(
        text.contains("saved!"),
        "status message should appear, got: {text:?}"
    );
}

#[test]
fn frame_system_popup() {
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.mode = PanelMode::Loading;
    app.panels.push(panel);
    app.focused_panel = 0;
    app.show_welcome = false;

    app.set_system_message_persistent(ChatMessage {
        role: MessageRole::System,
        content: "POPUP-MARKER".into(),
    });

    let text = render_screen(&mut app, 80, 40);
    assert!(
        text.contains("POPUP-MARKER"),
        "system popup should contain 'POPUP-MARKER', got: {text:?}"
    );
}

// ── T9 — Handler state tests (no TTY) ────────────────────────────────────────

#[tokio::test]
async fn handler_add_pushes_panel() {
    let mut app = App::new();
    let (tx, _rx) = mpsc::unbounded_channel();

    handle_command(
        &mut app,
        Command::AddAgent {
            agent: "claude".into(),
            image: None,
            tag: None,
            project: None,
            branch: None,
            name: None,
            auto_mode: false,
            interactive: false,
            prompt: None,
            model: None,
            use_env: vec![],
            env_file: None,
            run_as_root: false,
        },
        &tx,
    )
    .await;

    assert_eq!(app.panels.len(), 1, "should have 1 panel");
    // Panel stays in Loading mode (no console available)
    assert_eq!(app.panels[0].mode, PanelMode::Loading);
}

#[tokio::test]
async fn handler_close_hides_panel() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("claude"));
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();

    handle_command(&mut app, Command::Close { target: None }, &tx).await;

    assert!(!app.panels[0].visible, "panel should be hidden");
}

#[tokio::test]
async fn handler_open_shows_panel() {
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.visible = false;
    app.panels.push(panel);
    let (tx, _rx) = mpsc::unbounded_channel();

    handle_command(&mut app, Command::Open { target: None }, &tx).await;

    assert!(app.panels[0].visible, "panel should be visible");
}

#[tokio::test]
async fn handler_focus_sets_index() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("a"));
    app.panels.push(AgentPanel::new("b"));
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();

    handle_command(&mut app, Command::Focus { panel: 1 }, &tx).await;

    assert_eq!(app.focused_panel, 1, "focused panel should be 1");
}

#[tokio::test]
async fn handler_kill_removes_panel() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("claude"));
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();

    handle_command(&mut app, Command::Kill { panel: None }, &tx).await;

    assert!(app.panels.is_empty(), "all panels should be removed");
}

#[tokio::test]
async fn handler_env_set() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("claude"));
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();

    handle_command(
        &mut app,
        Command::Env {
            assignment: Some(("K".into(), "V".into())),
        },
        &tx,
    )
    .await;

    assert_eq!(
        app.panels[0].env.get("K"),
        Some(&"V".into()),
        "env should have K=V"
    );
}

#[tokio::test]
async fn handler_env_list_no_assignment() {
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.env.insert("EXISTING".into(), "value".into());
    app.panels.push(panel);
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();

    handle_command(
        &mut app,
        Command::Env { assignment: None },
        &tx,
    )
    .await;

    assert!(
        !app.panels[0].chat_history.is_empty(),
        "env list should push to chat_history"
    );
}

#[tokio::test]
async fn handler_theme_switch() {
    let mut app = App::new();
    let (tx, _rx) = mpsc::unbounded_channel();

    handle_command(
        &mut app,
        Command::Theme {
            name: Some("dracula".into()),
        },
        &tx,
    )
    .await;

    assert_eq!(
        app.theme_name,
        ThemeName::Dracula,
        "theme should be Dracula"
    );
}

#[tokio::test]
async fn handler_zoom_toggles() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("claude"));
    let (tx, _rx) = mpsc::unbounded_channel();

    handle_command(&mut app, Command::Zoom, &tx).await;
    assert!(app.zoomed, "zoom should be true after first toggle");

    handle_command(&mut app, Command::Zoom, &tx).await;
    assert!(!app.zoomed, "zoom should be false after second toggle");
}

#[tokio::test]
async fn handler_reconnect_sets_reconnecting() {
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.set_supervisor("sbox".into());
    app.panels.push(panel);
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();

    handle_command(&mut app, Command::Reconnect, &tx).await;

    assert!(app.panels[0].reconnecting, "panel should be reconnecting");
    assert_eq!(
        app.panels[0].mode,
        PanelMode::Loading,
        "panel mode should be Loading"
    );
}

#[tokio::test]
async fn handler_clearhistory_clears() {
    let mut app = App::new();
    app.command_history.push("/help");
    app.command_history.push("/quit");
    let (tx, _rx) = mpsc::unbounded_channel();

    handle_command(&mut app, Command::ClearHistory, &tx).await;

    assert!(
        app.command_history.navigate_up("").is_none(),
        "history should be empty"
    );
}

#[test]
fn handler_deprecated_subcommands_do_not_parse() {
    assert!(
        matches!(
            commands::parse_command_verbose("/mcp add x"),
            ParseResult::Err(_)
        ),
        "/mcp add should be Err"
    );
    assert!(
        matches!(
            commands::parse_command_verbose("/skills add x"),
            ParseResult::Err(_)
        ),
        "/skills add should be Err"
    );
    assert!(
        matches!(
            commands::parse_command_verbose("/agent set x"),
            ParseResult::Err(_)
        ),
        "/agent set should be Err"
    );
}

// ── T10 — Synthetic event loop test (AC5) ────────────────────────────────────

#[tokio::test]
async fn event_loop_100_synthetic_events() {
    use ratatui::crossterm::event::{
        KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers,
    };

    let mut app = App::new();
    let (tx, _rx) = mpsc::unbounded_channel();

    // Build 100+ synthetic events.
    let mut events: Vec<AppEvent> = Vec::new();

    // 20 Tick events
    for _ in 0..20 {
        events.push(AppEvent::Tick);
    }

    // 20 key-press events (typing 'x')
    for _ in 0..20 {
        events.push(AppEvent::Terminal(ratatui::crossterm::event::Event::Key(
            KeyEvent {
                code: KeyCode::Char('x'),
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            },
        )));
    }

    // 10 resize events
    for _ in 0..10 {
        events.push(AppEvent::Terminal(ratatui::crossterm::event::Event::Resize(
            120, 40,
        )));
    }

    // 10 focus events
    for _ in 0..10 {
        events.push(AppEvent::Terminal(ratatui::crossterm::event::Event::FocusGained));
        events.push(AppEvent::Terminal(ratatui::crossterm::event::Event::FocusLost));
    }

    // Type "/help" into the global input via keystrokes
    for c in "/help".chars() {
        events.push(AppEvent::Terminal(ratatui::crossterm::event::Event::Key(
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            },
        )));
    }
    // Press Enter to submit
    events.push(AppEvent::Terminal(ratatui::crossterm::event::Event::Key(
        KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        },
    )));

    // More ticks to fill up to 100+
    while events.len() < 100 {
        events.push(AppEvent::Tick);
    }

    // Process all events.
    for ev in events {
        let _ = handle_event(&mut app, ev, &tx).await;
    }

    // Render and verify the frame actually painted content.
    let text = render_screen(&mut app, 80, 40);
    assert!(
        text.contains("No agent panels")
            || text.contains("Getting Started")
            || text.contains("Available commands"),
        "render after 100 events should paint a known screen: {text:?}"
    );

    // At least one state mutation occurred.
    assert!(
        app.sidebar_tick_counter != 0 || !app.system_messages.is_empty(),
        "state should have mutated: tick_counter={}, system_messages={}",
        app.sidebar_tick_counter,
        app.system_messages.len(),
    );
}

// ── New command handlers (security refactor) ─────────────────────────────────

#[tokio::test]
async fn handler_diff_no_project_reports() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("claude"));
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_command(&mut app, Command::Diff { stat: false }, &tx).await;
    assert!(app.status_message.is_some(), "/diff should report when no clone");
}

#[tokio::test]
async fn handler_status_no_project_reports() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("claude"));
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_command(&mut app, Command::Status, &tx).await;
    assert!(app.status_message.is_some(), "/status should report when no clone");
}

#[tokio::test]
async fn handler_mounts_lists_without_panic() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("claude"));
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_command(&mut app, Command::Mounts, &tx).await;
    assert!(
        !app.panels[0].chat_history.is_empty(),
        "/mounts should push a message"
    );
}

#[tokio::test]
async fn handler_logs_no_supervisor_reports() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("claude"));
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_command(&mut app, Command::Logs { count: Some(10) }, &tx).await;
    assert!(app.status_message.is_some(), "/logs should report with no sandbox");
}

#[tokio::test]
async fn handler_exec_no_channel_reports() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("claude"));
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_command(&mut app, Command::Exec { args: vec!["echo".into(), "hi".into()] }, &tx).await;
    assert!(app.status_message.is_some(), "/exec should report with no exec channel");
}

#[tokio::test]
async fn handler_stop_no_supervisor_reports() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("claude"));
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_command(&mut app, Command::Stop { target: None }, &tx).await;
    assert!(app.status_message.is_some(), "/stop should report with no supervisor");
}

#[tokio::test]
async fn handler_discard_no_project_reports() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("claude"));
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_command(&mut app, Command::Discard, &tx).await;
    assert!(app.status_message.is_some(), "/discard should report when no clone");
}

#[tokio::test]
async fn handler_sync_dry_run_reports() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("claude"));
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_command(&mut app, Command::Sync { dry_run: true, action: None }, &tx).await;
    assert!(
        !app.panels[0].chat_history.is_empty(),
        "/sync --dry-run should push a message"
    );
}

#[tokio::test]
async fn handler_project_lists_or_reports() {
    let mut app = App::new();
    let (tx, _rx) = mpsc::unbounded_channel();
    // No target => list registered projects (system message) or a notice.
    handle_command(&mut app, Command::Project { target: None }, &tx).await;
    assert!(
        !app.system_messages.is_empty() || app.status_message.is_some(),
        "/project with no target should list or notice"
    );
}

#[tokio::test]
async fn handler_project_missing_path_reports() {
    let mut app = App::new();
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_command(
        &mut app,
        Command::Project { target: Some("/nonexistent/xyz".into()) },
        &tx,
    )
    .await;
    assert!(app.status_message.is_some(), "/project <bad path> should report");
}

// ── /diff + /discard against host-owned review state (no VM, no TTY) ────────

/// Build a source repo with a base commit plus an agent commit reachable via
/// `refs/nanosb/<id>`, and a clone of the source. Returns (source, clone, base).
fn make_review_fixture(
    root: &std::path::Path,
    id: &str,
) -> (std::path::PathBuf, std::path::PathBuf, String) {
    let git = |dir: &std::path::Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git spawn");
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        out
    };
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    git(&src, &["config", "user.email", "a@b.c"]);
    git(&src, &["config", "user.name", "t"]);
    std::fs::write(src.join("f.txt"), "one\n").unwrap();
    git(&src, &["add", "-A"]);
    git(&src, &["commit", "-qm", "base"]);
    let base = String::from_utf8(git(&src, &["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string();

    std::fs::write(src.join("f.txt"), "two\n").unwrap();
    git(&src, &["add", "-A"]);
    git(&src, &["commit", "-qm", "agent change"]);
    git(&src, &["update-ref", &format!("refs/nanosb/{id}"), "HEAD"]);
    git(&src, &["reset", "--hard", &base]);

    let clone = root.join("clone");
    git(
        root,
        &["clone", "-q", src.to_str().unwrap(), clone.to_str().unwrap()],
    );
    (src, clone, base)
}

fn panel_with_review(
    src: &std::path::Path,
    clone: &std::path::Path,
    id: &str,
    base: &str,
) -> AgentPanel {
    let mut panel = AgentPanel::new("claude");
    panel.sandbox_id_short = id.to_string();
    panel.base_commit = Some(base.to_string());
    panel.project_mount = Some(sandbox::ProjectMount {
        source_path: src.to_path_buf(),
        layout: sandbox::ProjectLayout::SingleRepo {
            repo_path: src.to_path_buf(),
            current_branch: "main".to_string(),
        },
        worktree_base: Some(clone.to_path_buf()),
        created_branches: vec![(src.to_path_buf(), format!("nanosb/{id}"))],
        deferred_branch: None,
    });
    panel
}

#[tokio::test]
async fn handler_diff_reads_host_owned_review_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let id = "sbx";
    let (src, clone, base) = make_review_fixture(tmp.path(), id);
    let mut app = App::new();
    let (tx, _rx) = mpsc::unbounded_channel::<AppEvent>();
    app.panels.push(panel_with_review(&src, &clone, id, &base));
    app.focused_panel = 0;

    handle_command(&mut app, Command::Diff { stat: false }, &tx).await;

    let msg = app.panels[0]
        .chat_history
        .last()
        .map(|m| m.content.clone())
        .unwrap_or_default();
    assert!(
        msg.contains("f.txt"),
        "expected the host-owned review diff to list f.txt, got: {msg:?}"
    );
}

#[tokio::test]
async fn handler_discard_falls_back_to_host_reset() {
    let tmp = tempfile::tempdir().unwrap();
    let id = "sbx";
    let (src, clone, base) = make_review_fixture(tmp.path(), id);
    std::fs::write(clone.join("f.txt"), "dirty\n").unwrap();

    let mut app = App::new();
    let (tx, _rx) = mpsc::unbounded_channel::<AppEvent>();
    app.panels.push(panel_with_review(&src, &clone, id, &base));
    app.focused_panel = 0;

    handle_command(&mut app, Command::Discard, &tx).await;

    let msg = app.panels[0]
        .chat_history
        .last()
        .map(|m| m.content.clone())
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        msg.contains("discard") || msg.contains("reset"),
        "unexpected /discard message"
    );
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&clone)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "clone working tree should be clean after /discard"
    );
}
