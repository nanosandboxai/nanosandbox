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
    terminal.draw(|f| super::renderer::render(f, app)).unwrap();
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
fn frame_agent_tabs() {
    let mut app = App::new();
    // Distinct multi-char names so a title match cannot be a substring of
    // unrelated screen text.
    for name in &["alpha-panel", "beta-panel", "gamma-panel"] {
        let mut panel = AgentPanel::new(name);
        panel.mode = PanelMode::Terminal;
        let mut term = SshTerminal::new(40, 12);
        term.process_bytes(b"active-term-content");
        panel.terminal = Some(term);
        app.panels.push(panel);
    }
    app.focused_panel = 0;
    app.show_welcome = false;

    let text = render_screen(&mut app, 80, 40);
    assert_eq!(
        app.panel_areas.len(),
        1,
        "only the active panel should be drawn as a panel"
    );
    for name in ["alpha-panel", "beta-panel", "gamma-panel"] {
        assert!(
            text.contains(name),
            "each agent tab should render, missing {name:?} in: {text:?}"
        );
    }
    assert!(
        text.contains("active-term-content"),
        "active panel terminal content should render, got: {text:?}"
    );
}

#[test]
fn frame_mcp_sidebar() {
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.mode = PanelMode::Loading;
    app.panels.push(panel);
    app.focused_panel = 0;
    app.show_welcome = false;
    app.show_sandbox_sidebar = false;
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
fn sandbox_sidebar_open_by_default() {
    let app = App::new();
    assert!(
        app.show_sandbox_sidebar,
        "sandbox sidebar must be open by default"
    );
}

#[test]
fn frame_sandbox_sidebar_shows_metrics() {
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.mode = PanelMode::Terminal;
    panel.metrics = Some(super::app::SandboxMetrics {
        cpu_percent: 12.0,
        memory_used_bytes: 1_500_000_000,
        memory_limit_bytes: 4 * 1024 * 1024 * 1024,
        disk_used_bytes: 820 * 1024 * 1024,
    });
    app.panels.push(panel);
    app.focused_panel = 0;
    app.show_welcome = false;
    app.show_sandbox_sidebar = true;

    let text = render_screen(&mut app, 120, 40);
    assert!(
        text.contains("cpu 12%"),
        "sidebar should render CPU percent, got: {text:?}"
    );
    assert!(
        text.contains("disk"),
        "sidebar should render disk usage, got: {text:?}"
    );
}

#[test]
fn frame_sandbox_sidebar_shows_host_capacity() {
    let mut app = App::new();
    app.host_capacity = Some(super::host_capacity::HostCapacity {
        cpu_count: 8,
        memory_total_bytes: Some(16 * 1024 * 1024 * 1024),
        memory_available_bytes: None,
        disk_free_bytes: Some(120 * 1024 * 1024 * 1024),
    });
    let mut panel = AgentPanel::new("claude");
    panel.mode = PanelMode::Loading;
    app.panels.push(panel);
    app.focused_panel = 0;
    app.show_welcome = false;
    app.show_sandbox_sidebar = true;

    let text = render_screen(&mut app, 120, 40);
    assert!(
        text.contains("8cpu"),
        "sidebar should render host CPU count, got: {text:?}"
    );
    assert!(
        text.contains("16G ram"),
        "sidebar should render host RAM, got: {text:?}"
    );
    assert!(
        text.contains("120G free"),
        "sidebar should render full host disk-free text without truncation, got: {text:?}"
    );
}

#[test]
fn frame_sandbox_sidebar_flags_over_budget() {
    let mut app = App::new();
    app.host_capacity = Some(super::host_capacity::HostCapacity {
        cpu_count: 4,
        memory_total_bytes: Some(2 * 1024 * 1024 * 1024),
        memory_available_bytes: Some(1024 * 1024 * 1024),
        disk_free_bytes: None,
    });
    let mut panel = AgentPanel::new("claude");
    panel.mode = PanelMode::Loading;
    panel.original_config = Some(
        sandbox::SandboxConfig::builder()
            .image("claude")
            .cpus(2)
            .memory_mb(4096)
            .build(),
    );
    app.panels.push(panel);
    app.focused_panel = 0;
    app.show_welcome = false;
    app.show_sandbox_sidebar = true;

    let text = render_screen(&mut app, 120, 40);
    assert!(
        text.contains("OVER RAM"),
        "sidebar should flag over-budget memory, got: {text:?}"
    );
}

#[test]
fn frame_sandbox_sidebar_shows_configured_limits_without_metrics() {
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.mode = PanelMode::Loading;
    panel.original_config = Some(
        sandbox::SandboxConfig::builder()
            .image("claude")
            .cpus(2)
            .memory_mb(4096)
            .build(),
    );
    app.panels.push(panel);
    app.focused_panel = 0;
    app.show_welcome = false;
    app.show_sandbox_sidebar = true;

    let text = render_screen(&mut app, 120, 40);
    assert!(
        text.contains("mem -/4.0G"),
        "sidebar should show configured limit when live metrics are absent, got: {text:?}"
    );
}

#[tokio::test]
async fn metrics_updated_applies_to_matching_panel() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.set_supervisor("sbx-1".to_string());
    app.panels.push(panel);
    app.metrics_poll_in_flight = true;

    let sample = super::app::SandboxMetrics {
        cpu_percent: 5.0,
        memory_used_bytes: 100,
        memory_limit_bytes: 200,
        disk_used_bytes: 300,
    };
    handle_event(
        &mut app,
        AppEvent::MetricsUpdated {
            samples: vec![("sbx-1".to_string(), sample)],
        },
        &tx,
    )
    .await;

    assert_eq!(app.panels[0].metrics, Some(sample));
    assert!(
        !app.metrics_poll_in_flight,
        "in-flight guard must clear after samples arrive"
    );
}

#[tokio::test]
async fn sandbox_and_mcp_sidebars_are_mutually_exclusive() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut app = App::new();

    handle_command(&mut app, Command::McpToggle, &tx).await;
    assert!(app.show_mcp_sidebar);
    assert!(!app.show_sandbox_sidebar);

    handle_command(&mut app, Command::Sandboxes, &tx).await;
    assert!(app.show_sandbox_sidebar);
    assert!(!app.show_mcp_sidebar);
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

    // With tabs there is a single panel area; its border is always accent.
    let backend0 = render_backend(&mut app, 80, 40);
    let buf0 = backend0.buffer();
    assert_eq!(app.panel_areas.len(), 1);
    let (_, area0) = app.panel_areas[0];
    assert_eq!(border_fg(buf0, area0), Some(app.theme.accent));

    // Switching focus keeps the single panel border accent and both tabs.
    app.focused_panel = 1;
    let backend1 = render_backend(&mut app, 80, 40);
    let buf1 = backend1.buffer();
    assert_eq!(app.panel_areas.len(), 1);
    let (_, area1) = app.panel_areas[0];
    assert_eq!(border_fg(buf1, area1), Some(app.theme.accent));

    let text = screen_text(buf1);
    assert!(
        text.contains("alpha"),
        "tab strip should still list alpha, got: {text:?}"
    );
    assert!(
        text.contains("beta"),
        "tab strip should still list beta, got: {text:?}"
    );
}

#[test]
fn frame_tab_switches_active_panel() {
    let mut app = App::new();
    for (name, content) in [("alpha", "AAA-content"), ("beta", "BBB-content")] {
        let mut panel = AgentPanel::new(name);
        panel.mode = PanelMode::Terminal;
        let mut term = SshTerminal::new(40, 12);
        term.process_bytes(content.as_bytes());
        panel.terminal = Some(term);
        app.panels.push(panel);
    }
    app.show_welcome = false;

    app.focused_panel = 0;
    let text0 = render_screen(&mut app, 80, 40);
    assert!(
        text0.contains("AAA-content"),
        "panel 0 content should render, got: {text0:?}"
    );
    assert!(
        !text0.contains("BBB-content"),
        "panel 1 content must not render while tab 0 is active, got: {text0:?}"
    );

    app.focused_panel = 1;
    let text1 = render_screen(&mut app, 80, 40);
    assert!(
        text1.contains("BBB-content"),
        "panel 1 content should render, got: {text1:?}"
    );
    assert!(
        !text1.contains("AAA-content"),
        "panel 0 content must not render while tab 1 is active, got: {text1:?}"
    );
    assert!(
        text1.contains("alpha"),
        "tab strip should list alpha, got: {text1:?}"
    );
    assert!(
        text1.contains("beta"),
        "tab strip should list beta, got: {text1:?}"
    );
}

#[test]
fn frame_sandbox_sidebar_still_present_with_tabs() {
    let mut app = App::new();
    for name in &["alpha-panel", "beta-panel"] {
        let mut panel = AgentPanel::new(name);
        panel.mode = PanelMode::Loading;
        app.panels.push(panel);
    }
    app.focused_panel = 0;
    app.show_welcome = false;
    app.show_sandbox_sidebar = true;

    let text = render_screen(&mut app, 120, 40);
    assert!(
        text.contains("Sandboxes"),
        "sandbox sidebar should show 'Sandboxes', got: {text:?}"
    );
    assert!(
        text.contains("alpha-panel"),
        "tab strip should list alpha-panel, got: {text:?}"
    );
    assert!(
        text.contains("beta-panel"),
        "tab strip should list beta-panel, got: {text:?}"
    );
}

#[test]
fn frame_tab_strip_top_padding() {
    let mut app = App::new();
    for name in &["alpha-panel", "beta-panel"] {
        let mut panel = AgentPanel::new(name);
        panel.mode = PanelMode::Terminal;
        let mut term = SshTerminal::new(40, 12);
        term.process_bytes(b"content");
        panel.terminal = Some(term);
        app.panels.push(panel);
    }
    app.focused_panel = 0;
    app.show_welcome = false;
    app.show_sandbox_sidebar = false;

    let backend = render_backend(&mut app, 80, 40);
    let buf = backend.buffer();

    // Row 0 is the blank top padding: no tab names, no box-drawing borders.
    let row0: String = (0..buf.area.width)
        .map(|x| buf.cell((x, 0)).unwrap().symbol())
        .collect();
    assert!(
        row0.trim().is_empty(),
        "row 0 should be blank padding, got: {row0:?}"
    );
    for ch in ['┌', '└', '│', '─'] {
        assert!(
            !row0.contains(ch),
            "row 0 must not contain box-drawing char {ch:?}, got: {row0:?}"
        );
    }

    // Tab names appear on row 1.
    let row1: String = (0..buf.area.width)
        .map(|x| buf.cell((x, 1)).unwrap().symbol())
        .collect();
    assert!(
        row1.contains("alpha-panel"),
        "tab strip should be on row 1, got: {row1:?}"
    );
    assert!(
        row1.contains("beta-panel"),
        "tab strip should be on row 1, got: {row1:?}"
    );
}

#[test]
fn frame_sidebar_aligns_with_active_panel() {
    let mut app = App::new();
    for name in &["alpha-panel", "beta-panel"] {
        let mut panel = AgentPanel::new(name);
        panel.mode = PanelMode::Loading;
        app.panels.push(panel);
    }
    app.focused_panel = 0;
    app.show_welcome = false;
    app.show_sandbox_sidebar = true;

    let backend = render_backend(&mut app, 120, 40);
    let buf = backend.buffer();

    // The active panel's border row must equal the sidebar's border row (row 2).
    assert_eq!(app.panel_areas.len(), 1);
    let panel_top = app.panel_areas[0].1.y - 1;
    assert_eq!(panel_top, 2, "panel border should start on row 2");

    // Sidebar left column = 70% of 120 = 84; its top border sits on the same row.
    let sidebar_x = 84;
    let corner = buf.cell((sidebar_x, panel_top)).unwrap().symbol();
    assert!(
        corner == "┌" || corner == "│",
        "sidebar border should start at ({sidebar_x}, {panel_top}), got {corner:?}"
    );

    // The sidebar title must not appear on the padding or tab rows.
    let row0: String = (0..buf.area.width)
        .map(|x| buf.cell((x, 0)).unwrap().symbol())
        .collect();
    let row1: String = (0..buf.area.width)
        .map(|x| buf.cell((x, 1)).unwrap().symbol())
        .collect();
    assert!(
        !row0.contains("Sandboxes"),
        "sidebar title must not be on row 0, got: {row0:?}"
    );
    assert!(
        !row1.contains("Sandboxes"),
        "sidebar title must not be on row 1, got: {row1:?}"
    );
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

/// Provide the provider credentials the `/add` validation layer requires so
/// the handler tests can create panels. The mutation is process-global and
/// applied once; tests run in parallel, so every reader expects these to exist.
fn ensure_test_agent_env() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::env::set_var("ANTHROPIC_API_KEY", "test-anthropic");
        std::env::set_var("OPENAI_API_KEY", "test-openai");
        std::env::set_var("GEMINI_API_KEY", "test-gemini");
        std::env::set_var("CURSOR_API_KEY", "test-cursor");
        std::env::set_var("GOOSE_PROVIDER", "anthropic");
        std::env::set_var("GOOSE_MODEL", "claude-sonnet-4-5-20250929");
    });
}

#[tokio::test]
async fn handler_add_pushes_panel() {
    ensure_test_agent_env();
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
            interactive: true,
            prompt: None,
            model: None,
            use_env: vec![],
            env_file: None,
            run_as_root: false,
            timeout: None,
            permissions: None,
        },
        &tx,
    )
    .await;

    assert_eq!(app.panels.len(), 1, "should have 1 panel");
    // Panel stays in Loading mode (no console available)
    assert_eq!(app.panels[0].mode, PanelMode::Loading);
}

#[tokio::test]
async fn add_agent_does_not_show_validation_popup() {
    ensure_test_agent_env();
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
            interactive: true,
            prompt: None,
            model: None,
            use_env: vec![],
            env_file: None,
            run_as_root: false,
            timeout: None,
            permissions: None,
        },
        &tx,
    )
    .await;

    assert!(
        app.system_messages.is_empty(),
        "add must not raise a validation popup, got {:?}",
        app.system_messages
    );
    assert_eq!(app.panels.len(), 1, "the panel should still be created");
}

const ALL_AGENTS: &[&str] = &[
    "claude", "codex", "goose", "cursor", "opencode", "aider", "gemini", "crush",
];

#[tokio::test]
async fn handler_add_all_agents_interactive_panel_state() {
    ensure_test_agent_env();
    for agent in ALL_AGENTS {
        let mut app = App::new();
        let (tx, _rx) = mpsc::unbounded_channel();

        handle_command(
            &mut app,
            Command::AddAgent {
                agent: agent.to_string(),
                image: None,
                tag: None,
                project: None,
                branch: None,
                name: None,
                auto_mode: false,
                interactive: true,
                prompt: None,
                model: None,
                use_env: vec![],
                env_file: None,
                run_as_root: false,
                timeout: None,
                permissions: None,
            },
            &tx,
        )
        .await;

        assert_eq!(app.panels.len(), 1, "{}: should have 1 panel", agent);
        let panel = &app.panels[0];
        assert!(panel.interactive, "{}: interactive should be true", agent);
        assert!(!panel.auto_mode, "{}: auto_mode should be false", agent);
        assert!(
            panel.headless_state.is_none(),
            "{}: headless_state should be None",
            agent
        );
    }
}

#[tokio::test]
async fn handler_add_all_agents_headless_panel_state() {
    ensure_test_agent_env();
    for agent in ALL_AGENTS {
        let mut app = App::new();
        let (tx, _rx) = mpsc::unbounded_channel();

        handle_command(
            &mut app,
            Command::AddAgent {
                agent: agent.to_string(),
                image: None,
                tag: None,
                project: None,
                branch: None,
                name: None,
                auto_mode: true,
                interactive: false,
                prompt: Some("x".into()),
                model: None,
                use_env: vec![],
                env_file: None,
                run_as_root: false,
                timeout: None,
                permissions: None,
            },
            &tx,
        )
        .await;

        assert_eq!(app.panels.len(), 1, "{}: should have 1 panel", agent);
        let panel = &app.panels[0];
        assert!(panel.auto_mode, "{}: auto_mode should be true", agent);
        assert!(!panel.interactive, "{}: interactive should be false", agent);
        assert!(
            panel.headless_state.is_some(),
            "{}: headless_state should be Some",
            agent
        );
    }
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

    handle_command(&mut app, Command::Env { assignment: None }, &tx).await;

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
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

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
        events.push(AppEvent::Terminal(
            ratatui::crossterm::event::Event::Resize(120, 40),
        ));
    }

    // 10 focus events
    for _ in 0..10 {
        events.push(AppEvent::Terminal(
            ratatui::crossterm::event::Event::FocusGained,
        ));
        events.push(AppEvent::Terminal(
            ratatui::crossterm::event::Event::FocusLost,
        ));
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
    assert!(
        app.status_message.is_some(),
        "/diff should report when no clone"
    );
}

#[tokio::test]
async fn handler_status_no_project_reports() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("claude"));
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_command(&mut app, Command::Status, &tx).await;
    assert!(
        app.status_message.is_some(),
        "/status should report when no clone"
    );
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
    assert!(
        app.status_message.is_some(),
        "/logs should report with no sandbox"
    );
}

#[tokio::test]
async fn handler_exec_no_channel_reports() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("claude"));
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_command(
        &mut app,
        Command::Exec {
            args: vec!["echo".into(), "hi".into()],
        },
        &tx,
    )
    .await;
    assert!(
        app.status_message.is_some(),
        "/exec should report with no exec channel"
    );
}

#[tokio::test]
async fn handler_stop_no_supervisor_reports() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("claude"));
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_command(&mut app, Command::Stop { target: None }, &tx).await;
    assert!(
        app.status_message.is_some(),
        "/stop should report with no supervisor"
    );
}

#[tokio::test]
async fn handler_discard_no_project_reports() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("claude"));
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_command(&mut app, Command::Discard, &tx).await;
    assert!(
        app.status_message.is_some(),
        "/discard should report when no clone"
    );
}

#[tokio::test]
async fn handler_sync_dry_run_reports() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("claude"));
    app.focused_panel = 0;
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_command(
        &mut app,
        Command::Sync {
            dry_run: true,
            action: None,
        },
        &tx,
    )
    .await;
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
        Command::Project {
            target: Some("/nonexistent/xyz".into()),
        },
        &tx,
    )
    .await;
    assert!(
        app.status_message.is_some(),
        "/project <bad path> should report"
    );
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
        &[
            "clone",
            "-q",
            src.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
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

// ── Stable panel_id routing (sandbox-mixing regression) ──────────────────────

#[tokio::test]
async fn event_routes_to_surviving_panel_after_middle_removal() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut app = App::new();
    // Three panels get stable ids 1, 2, 3.
    app.push_panel(AgentPanel::new("claude"));
    app.push_panel(AgentPanel::new("codex"));
    app.push_panel(AgentPanel::new("opencode"));
    assert_eq!(app.panels.len(), 3);

    // Remove the middle panel (id 2). Later panels shift down one index.
    app.panels.remove(1);
    assert_eq!(app.panels.len(), 2);
    assert_eq!(app.panels[0].panel_id, 1);
    assert_eq!(app.panels[1].panel_id, 3);

    // Deliver an event for the surviving panel id 3 (now at index 1).
    handle_event(
        &mut app,
        AppEvent::SandboxFailed {
            panel_id: 3,
            error: "boom".to_string(),
        },
        &tx,
    )
    .await;

    // It must land on the panel with id 3 (index 1), not the shifted one.
    assert_eq!(app.panels[1].loading_error.as_deref(), Some("boom"));
    assert_eq!(app.panels[0].loading_error, None);
}

#[tokio::test]
async fn event_for_removed_panel_id_is_dropped() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut app = App::new();
    app.push_panel(AgentPanel::new("claude"));
    app.push_panel(AgentPanel::new("codex"));
    app.push_panel(AgentPanel::new("opencode"));

    // Remove the middle panel (id 2).
    app.panels.remove(1);
    assert_eq!(app.panels.len(), 2);

    // Deliver an event for the removed panel's id (2). It must be dropped
    // without affecting any surviving panel.
    handle_event(
        &mut app,
        AppEvent::SandboxFailed {
            panel_id: 2,
            error: "boom".to_string(),
        },
        &tx,
    )
    .await;

    assert!(
        app.panels.iter().all(|p| p.loading_error.is_none()),
        "event for a removed panel id must not affect any surviving panel"
    );
}

#[tokio::test]
async fn terminal_data_routes_to_surviving_panel_after_middle_removal() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut app = App::new();
    app.push_panel(AgentPanel::new("claude"));
    app.push_panel(AgentPanel::new("codex"));
    app.push_panel(AgentPanel::new("opencode"));

    // Give each panel a terminal so TerminalData is processed.
    for p in app.panels.iter_mut() {
        p.mode = PanelMode::Terminal;
        p.terminal = Some(SshTerminal::new(80, 24));
    }

    // Remove the middle panel (id 2).
    app.panels.remove(1);
    assert_eq!(app.panels.len(), 2);

    // Send data to the surviving panel id 3 (now at index 1).
    handle_event(
        &mut app,
        AppEvent::TerminalData {
            panel_id: 3,
            data: b"hello".to_vec(),
        },
        &tx,
    )
    .await;

    // The panel with id 3 must have received the data.
    let screen1 = app.panels[1].terminal.as_ref().unwrap().screen();
    assert!(
        screen1.contents_between(0, 0, 1, 5).contains("hello"),
        "panel id 3 should receive its own terminal data"
    );
    // The panel with id 1 must NOT have received it.
    let screen0 = app.panels[0].terminal.as_ref().unwrap().screen();
    assert!(
        !screen0.contents_between(0, 0, 1, 5).contains("hello"),
        "panel id 1 must not receive another panel's terminal data"
    );
}

// ── Transient console read errors (retry, don't kill) ────────────────────────

/// Create (once per process) a fake "running" supervisor state under a temp
/// `NANOSB_HOME` so `SupervisorClient::is_running()` reports true for the
/// sandbox name `sbox-1` without a real VM. Kept for the process lifetime so
/// tests can run in parallel without racing on the env var.
fn ensure_fake_running_supervisor() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let home =
            std::env::temp_dir().join(format!("nanosb-tui-reconnect-{}", std::process::id()));
        let sbox = home.join("sandboxes").join("sbox-1");
        let _ = std::fs::create_dir_all(&sbox);
        let state = serde_json::json!({
            "sandbox_name": "sbox-1",
            "state": "running",
            "pid": null,
            "exit_code": null,
            "started_at": "0",
            "config_hash": "",
            "supervisor_pid": std::process::id() as i32,
        });
        let _ = std::fs::write(sbox.join("state.json"), state.to_string());
        std::env::set_var("NANOSB_HOME", home);
    });
}

#[tokio::test]
async fn console_read_error_keeps_panel_and_reconnects() {
    ensure_fake_running_supervisor();
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.set_supervisor("sbox-1".into());
    panel.mode = PanelMode::Terminal;
    panel.terminal = Some(SshTerminal::new(80, 24));
    app.push_panel(panel);
    let panel_id = app.panels[0].panel_id;

    handle_event(
        &mut app,
        AppEvent::ConsoleReadError {
            panel_id,
            error: "read: connection reset".into(),
        },
        &tx,
    )
    .await;

    assert_eq!(
        app.panels.len(),
        1,
        "a transient read error must not remove the panel"
    );
    assert!(
        app.panels[0].reconnecting,
        "panel should be marked as reconnecting after a transient read error"
    );
    assert_eq!(
        app.panels[0].reconnect_attempts, 1,
        "the first transient read error counts one reconnect attempt"
    );
    assert_eq!(
        app.panels[0].mode,
        PanelMode::Terminal,
        "panel stays in terminal mode while reconnecting"
    );
}

#[tokio::test]
async fn ssh_disconnected_keeps_panel_with_reason_when_supervisor_dead() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.set_supervisor("sbox-dead".into());
    panel.mode = PanelMode::Terminal;
    app.push_panel(panel);
    let panel_id = app.panels[0].panel_id;

    handle_event(
        &mut app,
        AppEvent::SshDisconnected {
            panel_id,
            error: Some("agent exited with code 1".into()),
        },
        &tx,
    )
    .await;

    assert_eq!(
        app.panels.len(),
        1,
        "a dead sandbox must keep the panel so the failure stays visible"
    );
    let exited = app.panels[0].exited.as_deref().unwrap_or("");
    assert!(
        exited.contains("agent exited with code 1"),
        "the exit reason must be recorded on the panel, got {exited:?}"
    );
}

#[tokio::test]
async fn agent_exit_keeps_frozen_screen_and_records_reason() {
    ensure_fake_running_supervisor();
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.mode = PanelMode::Terminal;
    let mut term = SshTerminal::new(40, 12);
    term.process_bytes(b"Error: OPENAI_API_KEY is not set\r\n");
    panel.terminal = Some(term);
    panel.set_supervisor("sbox-1".into());
    app.push_panel(panel);
    let panel_id = app.panels[0].panel_id;

    handle_event(
        &mut app,
        AppEvent::SshDisconnected {
            panel_id,
            error: Some("agent exited with code 1".into()),
        },
        &tx,
    )
    .await;

    assert_eq!(app.panels.len(), 1, "panel must be kept");
    assert!(
        app.panels[0].terminal.is_some(),
        "the frozen terminal screen must be retained so the error stays visible"
    );
    assert_eq!(
        app.panels[0].exited.as_deref(),
        Some("agent exited with code 1"),
        "the exit reason must be recorded"
    );

    let text = render_screen(&mut app, 80, 40);
    assert!(
        text.contains("OPENAI_API_KEY"),
        "the agent's real error output must render in the panel, got: {text:?}"
    );
    assert!(
        text.contains("agent exited with code 1"),
        "the exit reason banner must render, got: {text:?}"
    );
}

#[tokio::test]
async fn ssh_disconnected_keeps_panel_when_supervisor_alive() {
    ensure_fake_running_supervisor();
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut app = App::new();
    let mut panel = AgentPanel::new("goose");
    panel.set_supervisor("sbox-1".into());
    panel.mode = PanelMode::Terminal;
    app.push_panel(panel);
    let panel_id = app.panels[0].panel_id;

    handle_event(
        &mut app,
        AppEvent::SshDisconnected {
            panel_id,
            error: None,
        },
        &tx,
    )
    .await;

    assert_eq!(
        app.panels.len(),
        1,
        "a clean agent exit on a healthy supervisor must keep the sandbox panel"
    );
    assert_eq!(app.panels[0].mode, PanelMode::Loading);
    assert!(
        app.panels[0].loading_error.is_some(),
        "the panel should surface the agent exit reason"
    );
}

#[tokio::test]
async fn console_read_error_exhausted_attempts_closes_panel() {
    ensure_fake_running_supervisor();
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.set_supervisor("sbox-1".into());
    panel.mode = PanelMode::Terminal;
    app.push_panel(panel);
    let panel_id = app.panels[0].panel_id;

    // Even a healthy sandbox only gets a bounded retry budget.
    for _ in 0..super::run::MAX_RECONNECT_ATTEMPTS {
        handle_event(
            &mut app,
            AppEvent::ConsoleReadError {
                panel_id,
                error: "read: connection reset".into(),
            },
            &tx,
        )
        .await;
    }
    assert_eq!(
        app.panels.len(),
        1,
        "retries within the budget must keep the panel"
    );

    // One more transient read error exhausts the budget and closes the panel.
    handle_event(
        &mut app,
        AppEvent::ConsoleReadError {
            panel_id,
            error: "read: connection reset".into(),
        },
        &tx,
    )
    .await;

    assert!(
        app.panels.is_empty(),
        "exhausted reconnect attempts must close the panel"
    );
}

#[test]
fn frame_zoomed_still_shows_tabs() {
    let mut app = App::new();
    for name in &["alpha-panel", "beta-panel"] {
        let mut panel = AgentPanel::new(name);
        panel.mode = PanelMode::Loading;
        app.panels.push(panel);
    }
    app.focused_panel = 1;
    app.show_welcome = false;
    app.zoomed = true;

    let text = render_screen(&mut app, 100, 30);
    assert!(
        text.contains("alpha-panel"),
        "zoom must keep the tab strip (alpha-panel), got: {text:?}"
    );
    assert!(
        text.contains("beta-panel"),
        "zoom must keep the tab strip (beta-panel), got: {text:?}"
    );
}

#[test]
fn frame_panel_card_has_no_title_duplicate() {
    let mut app = App::new();
    let mut panel = AgentPanel::new("claude");
    panel.mode = PanelMode::Terminal;
    let mut term = SshTerminal::new(40, 12);
    term.process_bytes(b"hello-body");
    panel.terminal = Some(term);
    app.panels.push(panel);
    app.focused_panel = 0;
    app.show_welcome = false;
    app.show_sandbox_sidebar = false;

    let text = render_screen(&mut app, 80, 40);
    assert_eq!(
        text.matches("claude").count(),
        1,
        "the agent name must appear once (in the tab strip), not on the panel card border, got: {text:?}"
    );
}
