//! Renderer that draws the TUI frames.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};
use ratatui::Frame;

use super::app::{App, AgentPanel, InputFocus, MouseSelection, PanelMode, SidebarFilesTab};
use super::commands::autocomplete;
use super::grid::grid_dimensions;
use super::theme::{Theme, ThemeName};

/// Maximum number of visual lines the input area can grow to.
const MAX_INPUT_HEIGHT: u16 = 10;

/// Render a full TUI frame based on the current application state.
pub fn render(frame: &mut Frame, app: &mut App) {
    let theme = app.theme;

    // Paint the theme background on the entire frame.
    let bg = Block::default().style(Style::new().bg(theme.background).fg(theme.text));
    frame.render_widget(bg, frame.area());

    // Compute dynamic global input height.
    let global_content_width = (frame.area().width as usize).saturating_sub(3).max(1);
    let global_input_height = if app.input_focus == InputFocus::Global {
        (app.global_input.visual_line_count(global_content_width) as u16)
            .clamp(1, MAX_INPUT_HEIGHT)
    } else {
        1
    };

    let show_header = app.visible_panel_count() == 0;
    let is_branded = matches!(
        app.theme_name,
        ThemeName::Nanosandbox | ThemeName::NanosandboxLight
    );

    let (body_area, global_input_area, status_area) = if show_header {
        let header_height = 4;
        let [header_area, body_area, global_input_area, status_area] = Layout::vertical([
            Constraint::Length(header_height),
            Constraint::Fill(1),
            Constraint::Length(global_input_height),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        if is_branded {
            render_branded_header(frame, header_area, theme);
        } else {
            render_header(frame, header_area, theme);
        }
        (body_area, global_input_area, status_area)
    } else {
        let [body_area, global_input_area, status_area] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(global_input_height),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        (body_area, global_input_area, status_area)
    };

    render_global_input(frame, global_input_area, app);
    render_status_bar(frame, status_area, app);

    if app.visible_panel_count() == 0 {
        if is_branded {
            render_branded_welcome(frame, body_area, app);
        } else {
            render_welcome(frame, body_area, app);
        }
    } else if app.show_mcp_sidebar || app.show_sandbox_sidebar {
        let [panels_area, sidebar_area] = Layout::horizontal([
            Constraint::Percentage(70),
            Constraint::Percentage(30),
        ])
        .areas(body_area);

        render_panel_grid(frame, panels_area, app);
        if app.show_sandbox_sidebar {
            render_sandbox_sidebar(frame, sidebar_area, app);
        } else {
            render_mcp_sidebar(frame, sidebar_area, theme);
        }
    } else {
        render_panel_grid(frame, body_area, app);
    }

    // Render autocomplete popup for the global input bar (overlays on top of body).
    if app.input_focus == InputFocus::Global && app.global_input.text().starts_with('/') {
        render_autocomplete(frame, body_area, global_input_area, app.global_input.text(), app.autocomplete_index, theme);
    }

    // Render system message popup overlay when panels are open.
    if !app.panels.is_empty() && !app.system_messages.is_empty() {
        render_system_popup(frame, body_area, app, theme);
    }
}

/// Render the header with a Unicode box logo and sparkles.
fn render_header(frame: &mut Frame, area: Rect, theme: &Theme) {
    let b = Style::new().fg(theme.text_muted);
    let s = Style::new().fg(theme.accent);
    let w = Style::new().fg(theme.text).add_modifier(Modifier::BOLD);
    let header = Paragraph::new(vec![
        Line::from(vec![Span::styled("\u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2510}", b)]),
        Line::from(vec![
            Span::styled("\u{2502} ", b),
            Span::styled("\u{2726}", s),
            Span::styled(" ", b),
            Span::styled("\u{2726}", s),
            Span::styled(" ", b),
            Span::styled("\u{2726}", s),
            Span::styled(" \u{2502}", b),
        ]),
        Line::from(vec![
            Span::styled("\u{2502}  ", b),
            Span::styled("</>", w),
            Span::styled("  \u{2502}", b),
        ]),
        Line::from(vec![Span::styled("\u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2518}", b)]),
    ])
    .alignment(Alignment::Center);
    frame.render_widget(header, area);
}

/// Render the branded nanosandbox hero header with logo, title, and tagline.
fn render_branded_header(frame: &mut Frame, area: Rect, theme: &Theme) {
    let a = Style::new().fg(theme.accent).add_modifier(Modifier::BOLD);
    let w = Style::new().fg(theme.text).add_modifier(Modifier::BOLD);

    //  ╭─────╮
    //  │ </> │   NANOSANDBOX
    //  ╰─────╯   Sandboxes for AI Code Agents

    let pad = "  ";
    let gap = "   ";
    let lines = vec![
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}", a),
        ]),
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{2502} ", a),
            Span::styled("</>", a),
            Span::styled(" \u{2502}", a),
            Span::raw(gap),
            Span::styled("NANOSANDBOX", a),
        ]),
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}", a),
            Span::raw(gap),
            Span::styled("Sandboxes for AI Code Agents", w),
        ]),
        Line::from(""),
    ];

    let header = Paragraph::new(lines);
    frame.render_widget(header, area);
}

/// Render the branded welcome / Getting Started section for nanosandbox themes.
fn render_branded_welcome(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;

    if !app.system_messages.is_empty() {
        // System messages override the welcome content (e.g. /help output).
        let lines: Vec<Line> = app
            .system_messages
            .iter()
            .flat_map(|msg| {
                let style = Style::new().fg(theme.warning);
                msg.content
                    .lines()
                    .map(|line_text| Line::from(Span::styled(line_text, style)))
                    .collect::<Vec<_>>()
            })
            .collect();

        let paragraph = Paragraph::new(lines)
            .block(Block::default().borders(Borders::NONE))
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
        return;
    }

    let a = Style::new().fg(theme.accent);
    let ab = Style::new().fg(theme.accent).add_modifier(Modifier::BOLD);
    let m = Style::new().fg(theme.text_muted);
    let s = Style::new().fg(theme.success);
    let w = Style::new().fg(theme.warning);
    let pad = "  ";

    let lines = vec![
        // ┌─ Getting Started ───────
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{250c}\u{2500} ", a),
            Span::styled("Getting Started", ab),
        ]),
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{2502}", a),
        ]),
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{2502}  ", a),
            Span::styled("Spawn a sandbox with an AI agent:", m),
        ]),
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{2502}", a),
        ]),
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{2502}    ", a),
            Span::styled("/add claude", s),
        ]),
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{2502}    ", a),
            Span::styled("/add codex", s),
        ]),
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{2502}    ", a),
            Span::styled("/add goose", s),
        ]),
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{2502}", a),
        ]),
        // ├─ Quick Commands ───────
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{251c}\u{2500} ", a),
            Span::styled("Quick Commands", ab),
        ]),
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{2502}", a),
        ]),
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{2502}    ", a),
            Span::styled("/help", w),
            Span::styled("          full command list", m),
        ]),
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{2502}    ", a),
            Span::styled("/theme", w),
            Span::styled("         switch colour theme", m),
        ]),
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{2502}    ", a),
            Span::styled("/quit", w),
            Span::styled("          exit nanosandbox", m),
        ]),
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{2502}", a),
        ]),
        Line::from(vec![
            Span::raw(pad),
            Span::styled("\u{2514}\u{2500}\u{2500}\u{2500}", a),
        ]),
    ];

    let welcome = Paragraph::new(lines);
    frame.render_widget(welcome, area);
}

/// Render the persistent global input bar with multiline wrapping and real cursor.
fn render_global_input(frame: &mut Frame, area: Rect, app: &mut App) {
    let theme = app.theme;
    let is_focused = app.input_focus == InputFocus::Global;
    let prompt = "> ";
    let prompt_len = prompt.len() as u16;
    let content_width = (area.width.saturating_sub(prompt_len) as usize).max(1);

    // Cache width for key handler.
    app.last_global_input_width = content_width as u16;

    let visual_lines = app.global_input.visual_lines(content_width);

    // Compute scroll offset if content exceeds area height.
    let viewport_height = area.height as usize;
    let scroll_offset = if is_focused {
        let (cursor_row, _) = app.global_input.cursor_visual_position(content_width);
        if cursor_row >= viewport_height {
            cursor_row - viewport_height + 1
        } else {
            0
        }
    } else {
        0
    };

    let prompt_style = if is_focused {
        Style::new().fg(theme.accent)
    } else {
        Style::new().fg(theme.text_muted)
    };
    let text_style = if is_focused {
        Style::default()
    } else {
        Style::new().fg(theme.text_muted)
    };

    let mut lines: Vec<Line> = Vec::new();
    for (i, vl) in visual_lines.iter().enumerate().skip(scroll_offset).take(viewport_height) {
        let text_slice = &app.global_input.text()[vl.byte_start..vl.byte_end];
        let prefix = if i == 0 {
            Span::styled(prompt, prompt_style)
        } else {
            Span::styled("  ", prompt_style)
        };
        lines.push(Line::from(vec![prefix, Span::styled(text_slice, text_style)]));
    }

    // Handle empty input.
    if lines.is_empty() {
        if is_focused {
            lines.push(Line::from(vec![
                Span::styled(prompt, prompt_style),
                Span::styled("Type / for commands...", Style::new().fg(theme.text_muted)),
            ]));
        } else {
            lines.push(Line::from(Span::styled(prompt, prompt_style)));
        }
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);

    // Render software cursor when focused (steady, theme-aware block).
    if is_focused {
        let (cursor_row, cursor_col) = app.global_input.cursor_visual_position(content_width);
        let visual_row = cursor_row.saturating_sub(scroll_offset);
        let x = area.x + prompt_len + (cursor_col as u16).min(area.width.saturating_sub(prompt_len + 1));
        let y = area.y + visual_row as u16;
        if y < area.y + area.height && x < area.x + area.width {
            let buf = frame.buffer_mut();
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.bg = theme.accent;
                cell.fg = theme.background;
            }
        }
    }
}

/// Render the status bar with keybinding hints.
fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let hints = if app.visible_panel_count() == 0 {
        Line::from(vec![
            Span::styled(" /add <agent>", Style::new().fg(theme.accent)),
            Span::raw(" new panel  "),
            Span::styled("/quit", Style::new().fg(theme.accent)),
            Span::raw(" exit"),
        ])
    } else if app.input_focus == InputFocus::Global {
        let mut spans = vec![
            Span::styled(" Tab", Style::new().fg(theme.accent)),
            Span::raw(" panel focus  "),
            Span::styled("/kill", Style::new().fg(theme.accent)),
            Span::raw(" destroy  "),
            Span::styled("/sb", Style::new().fg(theme.accent)),
            Span::raw(" sandboxes  "),
            Span::styled("/add", Style::new().fg(theme.accent)),
            Span::raw(" new  "),
            Span::styled("/quit", Style::new().fg(theme.accent)),
            Span::raw(" exit  "),
            Span::styled("^F", Style::new().fg(theme.accent)),
            Span::raw(if app.zoomed { " restore" } else { " maximize" }),
        ];
        if app.zoomed {
            spans.push(Span::styled(
                format!("  [{}/{}]", app.focused_panel, app.visible_panel_count()),
                Style::new().fg(theme.warning),
            ));
        }
        Line::from(spans)
    } else {
        // Check if focused panel is in terminal mode.
        let in_terminal = app
            .panels
            .get(app.focused_panel)
            .is_some_and(|p| p.mode == PanelMode::Terminal);

        if in_terminal {
            let mut spans = vec![
                Span::styled(" ^G", Style::new().fg(theme.accent)),
                Span::raw(" global bar  "),
                Span::styled("Tab", Style::new().fg(theme.accent)),
                Span::raw(" next panel  "),
                Span::styled("SSH Terminal", Style::new().fg(theme.success)),
                Span::raw("  "),
                Span::styled("^F", Style::new().fg(theme.accent)),
                Span::raw(if app.zoomed { " restore" } else { " maximize" }),
            ];
            if app.zoomed {
                spans.push(Span::styled(
                    format!("  [{}/{}]", app.focused_panel, app.visible_panel_count()),
                    Style::new().fg(theme.warning),
                ));
            }
            Line::from(spans)
        } else {
            // Loading mode
            let mut spans = vec![
                Span::styled(" Esc", Style::new().fg(theme.accent)),
                Span::raw(" global bar  "),
                Span::styled("Tab", Style::new().fg(theme.accent)),
                Span::raw(" next panel  "),
                Span::styled("Loading...", Style::new().fg(theme.warning)),
                Span::raw("  "),
                Span::styled("^F", Style::new().fg(theme.accent)),
                Span::raw(if app.zoomed { " restore" } else { " maximize" }),
            ];
            if app.zoomed {
                spans.push(Span::styled(
                    format!("  [{}/{}]", app.focused_panel, app.visible_panel_count()),
                    Style::new().fg(theme.warning),
                ));
            }
            Line::from(spans)
        }
    };

    // If there's a temporary status message, show it instead of hints.
    let line = if let Some((ref msg, _)) = app.status_message {
        Line::from(Span::styled(
            format!(" {}", msg),
            Style::new().fg(theme.warning),
        ))
    } else {
        hints
    };

    let bar = Paragraph::new(line).style(Style::new().bg(theme.status_bar_bg));
    frame.render_widget(bar, area);
}

/// Render the welcome screen shown when no panels exist.
/// The global input bar is rendered separately by `render()`.
fn render_welcome(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    if app.system_messages.is_empty() {
        let lines = vec![
            Line::from(""),
            Line::from("No agent panels are open."),
            Line::from(""),
            Line::from(vec![
                Span::raw("Type "),
                Span::styled("/add <agent>", Style::new().fg(theme.success)),
                Span::raw(" to spawn a new sandbox panel."),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw("Examples: "),
                Span::styled("/add claude", Style::new().fg(theme.warning)),
                Span::raw("  "),
                Span::styled("/add codex", Style::new().fg(theme.warning)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw("Type "),
                Span::styled("/help", Style::new().fg(theme.success)),
                Span::raw(" for a full list of commands."),
            ]),
        ];

        let welcome = Paragraph::new(lines)
            .alignment(Alignment::Center)
            .block(Block::default());
        frame.render_widget(welcome, area);
    } else {
        let lines: Vec<Line> = app
            .system_messages
            .iter()
            .flat_map(|msg| {
                let style = Style::new().fg(theme.warning);
                msg.content
                    .lines()
                    .map(|line_text| Line::from(Span::styled(line_text, style)))
                    .collect::<Vec<_>>()
            })
            .collect();

        let paragraph = Paragraph::new(lines)
            .block(Block::default().borders(Borders::NONE))
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
    }
}

/// Render the MCP management sidebar.
fn render_mcp_sidebar(frame: &mut Frame, area: Rect, theme: &Theme) {
    let lines = vec![
        Line::from(Span::styled(
            "MCP Servers",
            Style::new()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("/mcp list", Style::new().fg(theme.success)),
            Span::raw("  list servers"),
        ]),
        Line::from(vec![
            Span::styled("/mcp add", Style::new().fg(theme.success)),
            Span::raw("   add server"),
        ]),
        Line::from(vec![
            Span::styled("/mcp remove", Style::new().fg(theme.success)),
            Span::raw(" remove server"),
        ]),
        Line::from(vec![
            Span::styled("/mcp enable", Style::new().fg(theme.success)),
            Span::raw(" enable server"),
        ]),
        Line::from(vec![
            Span::styled("/mcp disable", Style::new().fg(theme.success)),
            Span::raw(" disable"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Press /mcp to toggle this sidebar.",
            Style::new().fg(theme.text_muted),
        )),
    ];

    let sidebar = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(theme.text_muted))
                .title(" MCP "),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(sidebar, area);
}

/// Render the sandbox sidebar with a sandboxes list and files section.
fn render_sandbox_sidebar(frame: &mut Frame, area: Rect, app: &App) {
    // Split sidebar into two sections: sandboxes (top) and files (bottom).
    let has_files = !app.sidebar_modified_files.is_empty() || !app.sidebar_committed_files.is_empty();
    let chunks = if has_files {
        Layout::vertical([
            Constraint::Percentage(40),
            Constraint::Percentage(60),
        ])
        .split(area)
    } else {
        // No files: give all space to sandboxes
        Layout::vertical([
            Constraint::Percentage(100),
            Constraint::Min(0),
        ])
        .split(area)
    };

    // ── Top section: Sandbox list ──
    render_sandbox_list(frame, chunks[0], app);

    // ── Bottom section: Files (Modified / Committed tabs) ──
    if has_files {
        render_files_section(frame, chunks[1], app);
    }
}

/// Render the sandbox list section of the sidebar.
fn render_sandbox_list(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let inner_height = area.height.saturating_sub(2) as usize; // border top + bottom
    let mut lines = Vec::new();

    if app.panels.is_empty() {
        lines.push(Line::from(Span::styled(
            "No sandboxes running.",
            Style::new().fg(theme.text_muted),
        )));
    } else {
        for (i, panel) in app.panels.iter().enumerate() {
            let is_focused = i == app.focused_panel;

            let status = if !panel.visible {
                Span::styled("◻ ", Style::new().fg(theme.text_muted))
            } else if panel.mode == PanelMode::Terminal {
                Span::styled("● ", Style::new().fg(theme.success))
            } else if panel.sandbox.is_some() {
                Span::styled("◌ ", Style::new().fg(theme.warning))
            } else {
                Span::styled("○ ", Style::new().fg(theme.text_muted))
            };

            let name_style = if !panel.visible {
                Style::new().fg(theme.text_muted)
            } else if is_focused {
                Style::new().fg(theme.text).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(theme.text)
            };

            let sid = if panel.sandbox_id_short.is_empty() {
                String::new()
            } else {
                format!(" {}", panel.sandbox_id_short)
            };

            let focus_marker = if is_focused { " *" } else { "" };

            let sync_label = if panel.project_mount.is_some() {
                let is_syncing = panel.sync_override
                    .unwrap_or(app.settings.gitsync.auto_sync);
                if is_syncing {
                    Span::styled(" [sync]", Style::new().fg(theme.success))
                } else {
                    Span::styled(" [clone]", Style::new().fg(theme.text_muted))
                }
            } else {
                Span::raw("")
            };

            let hidden_label = if !panel.visible {
                Span::styled(" [hidden]", Style::new().fg(theme.text_muted))
            } else {
                Span::raw("")
            };

            lines.push(Line::from(vec![
                Span::raw(format!(" [{}] ", i)),
                status,
                Span::styled(panel.display_name.as_deref().unwrap_or(&panel.agent_name), name_style),
                Span::styled(sid, Style::new().fg(theme.text_muted)),
                sync_label,
                hidden_label,
                Span::styled(focus_marker, Style::new().fg(theme.accent)),
            ]));
        }
    }

    // Add help hint if there's space
    let total_lines = lines.len();
    if total_lines + 2 <= inner_height {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "/sb to toggle",
            Style::new().fg(theme.text_muted),
        )));
    }

    let scroll = app.sidebar_sandbox_scroll.min(
        total_lines.saturating_sub(inner_height),
    ) as u16;

    let border_style = if !app.sidebar_files_focused {
        Style::new().fg(theme.accent)
    } else {
        Style::new().fg(theme.text_muted)
    };

    let sidebar = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(" Sandboxes "),
        )
        .scroll((scroll, 0));
    frame.render_widget(sidebar, area);
}

/// Render the files section of the sidebar with Modified/Committed tabs.
fn render_files_section(frame: &mut Frame, area: Rect, app: &App) {
    let theme = app.theme;
    let inner_height = area.height.saturating_sub(2) as usize; // border top + bottom

    // Build tab header line.
    let mod_count = app.sidebar_modified_files.len();
    let com_count = app.sidebar_committed_files.len();

    let mod_label = format!(" Modified ({}) ", mod_count);
    let com_label = format!(" Committed ({}) ", com_count);

    let (mod_style, com_style) = match app.sidebar_files_tab {
        SidebarFilesTab::Modified => (
            Style::new().fg(theme.warning).add_modifier(Modifier::BOLD),
            Style::new().fg(theme.text_muted),
        ),
        SidebarFilesTab::Committed => (
            Style::new().fg(theme.text_muted),
            Style::new().fg(theme.success).add_modifier(Modifier::BOLD),
        ),
    };

    let tab_line = Line::from(vec![
        Span::styled(mod_label, mod_style),
        Span::styled(com_label, com_style),
    ]);

    // Build file list based on active tab.
    let (files, scroll_offset) = match app.sidebar_files_tab {
        SidebarFilesTab::Modified => (&app.sidebar_modified_files, app.sidebar_files_scroll),
        SidebarFilesTab::Committed => (&app.sidebar_committed_files, app.sidebar_committed_scroll),
    };

    let mut lines: Vec<Line> = vec![tab_line];

    for entry in files.iter() {
        let line = match app.sidebar_files_tab {
            SidebarFilesTab::Modified => {
                // git status --porcelain format: "XY filename"
                let (status_str, filename) = if entry.len() > 3 {
                    (&entry[..2], entry[3..].trim())
                } else {
                    (entry.as_str(), "")
                };

                let status_color = match status_str.trim() {
                    "M" | " M" | "MM" => theme.warning,
                    "A" | " A" => theme.success,
                    "D" | " D" => theme.error,
                    "R" => theme.info,
                    "??" => theme.text_muted,
                    _ => theme.text,
                };

                Line::from(vec![
                    Span::styled(
                        format!(" {} ", status_str),
                        Style::new().fg(status_color),
                    ),
                    Span::styled(filename, Style::new().fg(theme.text)),
                ])
            }
            SidebarFilesTab::Committed => {
                Line::from(Span::styled(
                    format!("  {}", entry),
                    Style::new().fg(theme.success),
                ))
            }
        };
        lines.push(line);
    }

    // Total lines includes tab header.
    let total_content = files.len() + 1; // +1 for tab header
    let scroll = scroll_offset.min(
        total_content.saturating_sub(inner_height),
    ) as u16;

    // Show scroll indicator if content overflows.
    if total_content > inner_height {
        let remaining = total_content.saturating_sub(inner_height + scroll_offset);
        if remaining > 0 {
            let hint = format!("  \u{2193} {} more", remaining);
            lines.push(Line::from(Span::styled(hint, Style::new().fg(theme.text_muted))));
        }
    }

    let border_style = if app.sidebar_files_focused {
        Style::new().fg(theme.accent)
    } else {
        Style::new().fg(theme.text_muted)
    };

    let title = match app.sidebar_files_tab {
        SidebarFilesTab::Modified => " Files ",
        SidebarFilesTab::Committed => " Files ",
    };

    let widget = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(title),
        )
        .scroll((scroll, 0));
    frame.render_widget(widget, area);
}

/// Render the panel grid based on the number of visible panels.
fn render_panel_grid(frame: &mut Frame, area: Rect, app: &mut App) {
    // Collect visible panel indices.
    let visible_indices: Vec<usize> = app.panels.iter().enumerate()
        .filter(|(_, p)| p.visible)
        .map(|(i, _)| i)
        .collect();

    let visible_count = visible_indices.len();
    if visible_count == 0 {
        return;
    }

    // Clear cached panel areas before re-recording.
    app.panel_areas.clear();

    let theme = app.theme;
    let selection = app.mouse_selection.clone();
    let sel_ref = selection.as_ref();

    // Determine if panel indices should be shown (duplicate display names).
    let show_index = {
        let mut seen = std::collections::HashSet::new();
        visible_indices.iter().any(|&i| {
            let name = app.panels[i].display_name.as_deref().unwrap_or(&app.panels[i].agent_name);
            !seen.insert(name)
        })
    };

    // Zoomed mode: render only the focused panel at full width.
    if app.zoomed {
        let idx = app.focused_panel;
        if idx < app.panels.len() && app.panels[idx].visible {
            let is_focused = true;
            let is_input_focused = app.input_focus == InputFocus::Panel;
            let ac_idx = if is_input_focused {
                app.autocomplete_index
            } else {
                None
            };
            render_panel(
                frame,
                area,
                &mut app.panels[idx],
                idx,
                is_focused,
                is_input_focused,
                ac_idx,
                theme,
                &mut app.panel_areas,
                sel_ref,
                show_index,
            );
        }
        return;
    }

    let (rows, cols) = grid_dimensions(visible_count);
    if rows == 0 || cols == 0 {
        return;
    }

    // Split vertically into rows.
    let row_constraints: Vec<Constraint> = (0..rows)
        .map(|_| Constraint::Ratio(1, rows as u32))
        .collect();
    let row_areas = Layout::vertical(row_constraints).split(area);

    let mut vi = 0; // index into visible_indices

    for row in 0..rows {
        if vi >= visible_count {
            break;
        }

        // Determine how many columns this row actually has.
        let cols_in_row = cols.min(visible_count - vi);

        let col_constraints: Vec<Constraint> = (0..cols_in_row)
            .map(|_| Constraint::Ratio(1, cols_in_row as u32))
            .collect();
        let col_areas = Layout::horizontal(col_constraints).split(row_areas[row]);

        for col in 0..cols_in_row {
            if vi < visible_count {
                let panel_idx = visible_indices[vi];
                let is_focused = panel_idx == app.focused_panel;
                let is_input_focused = is_focused && app.input_focus == InputFocus::Panel;
                let ac_idx = if is_input_focused {
                    app.autocomplete_index
                } else {
                    None
                };
                render_panel(
                    frame,
                    col_areas[col],
                    &mut app.panels[panel_idx],
                    panel_idx,
                    is_focused,
                    is_input_focused,
                    ac_idx,
                    theme,
                    &mut app.panel_areas,
                    sel_ref,
                    show_index,
                );
                vi += 1;
            }
        }
    }
}

/// Render a single agent panel.
fn render_panel(
    frame: &mut Frame,
    area: Rect,
    panel: &mut AgentPanel,
    index: usize,
    is_focused: bool,
    _is_input_focused: bool,
    _autocomplete_index: Option<usize>,
    theme: &Theme,
    panel_areas: &mut Vec<(usize, Rect)>,
    selection: Option<&MouseSelection>,
    show_index: bool,
) {
    // Status indicator.
    let status_indicator = if panel.mode == PanelMode::Headless {
        Span::styled("\u{25b6} ", Style::new().fg(theme.accent))
    } else if panel.mode == PanelMode::Loading {
        Span::styled("\u{25cc} ", Style::new().fg(theme.warning))
    } else if panel.sandbox.is_some() {
        Span::styled("\u{25cf} ", Style::new().fg(theme.success))
    } else {
        Span::styled("\u{25cb} ", Style::new().fg(theme.text_muted))
    };

    // Build title line: status dot + display name + optional index.
    let display = panel.display_name.as_deref().unwrap_or(&panel.agent_name);

    let mut title_spans = vec![
        Span::raw(" "),
        status_indicator,
        Span::styled(display, Style::new().add_modifier(Modifier::BOLD)),
    ];

    if show_index {
        title_spans.push(Span::styled(
            format!(" [{}]", index),
            Style::new().fg(theme.text_muted),
        ));
    }

    title_spans.push(Span::raw(" "));

    let title = Line::from(title_spans);

    let border_color = if is_focused {
        theme.accent
    } else {
        theme.text_muted
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background))
        .border_style(Style::new().fg(border_color).bg(theme.background))
        .title(title);

    let inner_area = block.inner(area);
    frame.render_widget(block, area);

    // Record inner area for mouse hit-testing.
    panel_areas.push((index, inner_area));

    if inner_area.height < 2 || inner_area.width < 2 {
        return;
    }

    // Track panel size unconditionally so last_terminal_size is correct
    // even during Loading mode (before SSH connects).
    let cols = inner_area.width;
    let rows = inner_area.height;
    if (cols, rows) != panel.last_terminal_size {
        panel.last_terminal_size = (cols, rows);
        if let Some(ref mut term) = panel.terminal {
            term.resize(cols, rows);
        }
        if let Some(ref handle) = panel.terminal_handle {
            let _ = handle.resize_tx.send((cols, rows));
        }
    }

    // Headless mode: render structured output from NDJSON stream.
    if panel.mode == PanelMode::Headless {
        render_headless_panel(frame, inner_area, panel, theme);
        return;
    }

    // Terminal mode: render PseudoTerminal widget for the entire inner area.
    if panel.mode == PanelMode::Terminal {
        if let Some(ref mut term) = panel.terminal {
            let pseudo_term = tui_term::widget::PseudoTerminal::new(term.screen());
            frame.render_widget(pseudo_term, inner_area);

            // Show scrollbar when scrolled up from live view.
            if term.is_scrolled_up() {
                let max = term.scrollback_max();
                render_scrollbar(frame, inner_area, term.scroll_offset(), max, theme);
            }

            // Overlay selection highlighting if this panel has an active selection.
            if let Some(sel) = selection {
                if sel.panel_idx == index {
                    let (start, end) = sel.normalized();
                    render_selection_overlay(frame, inner_area, start, end, theme);
                }
            }

            // Render overlay notification banner (upload success/failure).
            if let Some((ref msg, is_error, _)) = panel.notification {
                render_panel_notification(frame, inner_area, msg, is_error, theme);
            }

            // Place cursor at the terminal's cursor position when focused.
            // Hide cursor when viewing scrollback (position is meaningless).
            if is_focused && !term.is_scrolled_up() {
                let cursor = term.screen().cursor_position();
                let x = inner_area.x + cursor.1;
                let y = inner_area.y + cursor.0;
                if x < inner_area.x + inner_area.width && y < inner_area.y + inner_area.height {
                    frame.set_cursor_position((x, y));
                }
            }
        }
        return;
    }

    // Loading mode: render centered logo with animated border sweep.
    render_loading_animation(frame, inner_area, panel, theme);
}

/// Render a headless panel with structured output from NDJSON stream events.
///
/// Layout (3 sections):
///   1. Header — task description + status + elapsed time
///   2. Tools  — scrollable list of tool call log entries
///   3. Output — scrollable agent text output
fn render_headless_panel(
    frame: &mut Frame,
    area: Rect,
    panel: &AgentPanel,
    theme: &Theme,
) {
    let state = match &panel.headless_state {
        Some(s) => s,
        None => {
            let msg = Paragraph::new("Waiting for headless stream...")
                .style(Style::new().fg(theme.text_muted))
                .alignment(Alignment::Center);
            frame.render_widget(msg, area);
            return;
        }
    };

    // Compute elapsed time.
    let elapsed = state.started_at.elapsed();
    let mins = elapsed.as_secs() / 60;
    let secs = elapsed.as_secs() % 60;
    let elapsed_str = format!("{mins}:{secs:02}");

    // Divide area: header (3 lines), tools (dynamic), output (rest).
    let tool_height = if state.tool_calls.is_empty() {
        0
    } else {
        (state.tool_calls.len() as u16 + 2).min(area.height / 3) // +2 for border
    };

    let constraints = if tool_height > 0 {
        vec![
            Constraint::Length(3),
            Constraint::Length(tool_height),
            Constraint::Min(3),
        ]
    } else {
        vec![
            Constraint::Length(3),
            Constraint::Length(0),
            Constraint::Min(3),
        ]
    };

    let sections = Layout::vertical(constraints).split(area);

    // --- Section 1: Header ---
    let status_color = match state.status.as_str() {
        "thinking" | "tool_use" => theme.accent,
        "completed" => theme.success,
        "error" => theme.error,
        _ if state.status.starts_with("running") => theme.accent,
        _ => theme.text_muted,
    };

    let task_display = if state.task.len() > (area.width as usize).saturating_sub(20) {
        let max = (area.width as usize).saturating_sub(23);
        format!("{}...", &state.task[..max.min(state.task.len())])
    } else {
        state.task.clone()
    };

    let header_lines = vec![
        Line::from(vec![
            Span::styled("Task: ", Style::new().fg(theme.text_muted)),
            Span::styled(task_display, Style::new().fg(theme.text)),
        ]),
        Line::from(vec![
            Span::styled("Status: ", Style::new().fg(theme.text_muted)),
            Span::styled(&state.status, Style::new().fg(status_color)),
            Span::styled(format!("  [{elapsed_str}]"), Style::new().fg(theme.text_muted)),
        ]),
    ];

    let header = Paragraph::new(header_lines)
        .style(Style::new().bg(theme.background));
    frame.render_widget(header, sections[0]);

    // --- Section 2: Tool calls ---
    if tool_height > 0 && !state.tool_calls.is_empty() {
        let items: Vec<ListItem> = state
            .tool_calls
            .iter()
            .rev()
            .take(tool_height.saturating_sub(2) as usize)
            .map(|tc| {
                let icon = match tc.status.as_str() {
                    "running" => Span::styled("~ ", Style::new().fg(theme.warning)),
                    "done" => Span::styled("+ ", Style::new().fg(theme.success)),
                    "error" => Span::styled("x ", Style::new().fg(theme.error)),
                    _ => Span::styled("  ", Style::new().fg(theme.text_muted)),
                };

                let summary = if tc.input_summary.is_empty() {
                    tc.tool_name.clone()
                } else {
                    format!("{}: {}", tc.tool_name, tc.input_summary)
                };

                ListItem::new(Line::from(vec![
                    icon,
                    Span::styled(summary, Style::new().fg(theme.text)),
                ]))
            })
            .collect();

        let tools_block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::new().fg(theme.text_muted))
            .title(Span::styled(
                " Tools ",
                Style::new().fg(theme.text_muted),
            ));

        let tools_list = List::new(items)
            .block(tools_block)
            .style(Style::new().bg(theme.background));
        frame.render_widget(tools_list, sections[1]);
    }

    // --- Section 3: Agent text output ---
    let output_block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::new().fg(theme.text_muted))
        .title(Span::styled(
            " Output ",
            Style::new().fg(theme.text_muted),
        ));

    let text = if !state.agent_text.is_empty() {
        state.agent_text.clone()
    } else if !state.raw_lines.is_empty() {
        // Events received but no text yet — agent is initializing
        "Agent initializing...".to_string()
    } else {
        "Waiting for agent output...".to_string()
    };

    // Auto-scroll to bottom when enabled: estimate wrapped line count and scroll to end.
    // The output block has a top border (1 line), so visible height is height - 1.
    let visible_height = sections[2].height.saturating_sub(1) as usize;
    let output_width = sections[2].width.saturating_sub(2).max(1) as usize;
    let total_lines: usize = text.lines().map(|l| {
        let len = l.len().max(1); // empty lines count as 1
        (len + output_width - 1) / output_width
    }).sum::<usize>().max(1);

    let scroll = if state.auto_scroll && total_lines > visible_height {
        (total_lines - visible_height) as u16
    } else {
        state.scroll_offset
    };

    let output = Paragraph::new(text)
        .block(output_block)
        .style(Style::new().fg(theme.text).bg(theme.background))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(output, sections[2]);
}

/// Render selection highlighting by modifying buffer cells in the selected range.
fn render_selection_overlay(
    frame: &mut Frame,
    inner_area: Rect,
    start: (u16, u16),
    end: (u16, u16),
    theme: &Theme,
) {
    let buf = frame.buffer_mut();

    for row in start.0..=end.0 {
        let abs_y = inner_area.y + row;
        if abs_y >= inner_area.y + inner_area.height {
            break;
        }

        let col_start = if row == start.0 { start.1 } else { 0 };
        let col_end = if row == end.0 {
            end.1
        } else {
            inner_area.width.saturating_sub(1)
        };

        for col in col_start..=col_end {
            let abs_x = inner_area.x + col;
            if abs_x >= inner_area.x + inner_area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((abs_x, abs_y)) {
                cell.fg = theme.selection_fg;
                cell.bg = theme.selection_bg;
            }
        }
    }
}

/// Render a notification banner overlaid at the bottom of a terminal panel.
///
/// Draws a single-line banner with the message, replacing any previous content
/// on that row. Success messages use the theme accent; errors use red.
fn render_panel_notification(
    frame: &mut Frame,
    inner_area: Rect,
    msg: &str,
    is_error: bool,
    theme: &Theme,
) {
    if inner_area.height < 2 || inner_area.width < 4 {
        return;
    }

    // Place the banner on the last row of the terminal area.
    let banner_area = Rect {
        x: inner_area.x,
        y: inner_area.y + inner_area.height - 1,
        width: inner_area.width,
        height: 1,
    };

    let fg = if is_error {
        ratatui::style::Color::White
    } else {
        theme.text
    };
    let bg = if is_error {
        theme.error
    } else {
        theme.accent
    };

    // Truncate message to fit and pad to fill the full width.
    let max_len = banner_area.width as usize;
    let icon = if is_error { " \u{2717} " } else { " \u{2713} " }; // ✗ / ✓
    let display = format!("{}{}", icon, msg);
    let truncated: String = display.chars().take(max_len).collect();

    let span = Span::styled(
        format!("{:<width$}", truncated, width = max_len),
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
    );
    let paragraph = Paragraph::new(Line::from(span));
    frame.render_widget(Clear, banner_area);
    frame.render_widget(paragraph, banner_area);
}

/// Render a vertical scrollbar on the right edge of a terminal panel.
fn render_scrollbar(
    frame: &mut Frame,
    inner_area: Rect,
    scroll_offset: usize,
    scrollback_max: usize,
    theme: &Theme,
) {
    if inner_area.height < 3 || scrollback_max == 0 {
        return;
    }

    // Scrollbar position: 0 = top (most scrolled up), max = bottom (live view).
    // Our scroll_offset: 0 = live view (bottom), max = most scrolled up (top).
    // Invert so the thumb is at the top when fully scrolled up.
    let position = scrollback_max.saturating_sub(scroll_offset);

    let mut state = ScrollbarState::new(scrollback_max).position(position);

    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some("│"))
        .thumb_symbol("█")
        .track_style(Style::default().fg(theme.text_muted))
        .thumb_style(Style::default().fg(theme.accent));

    frame.render_stateful_widget(scrollbar, inner_area, &mut state);
}

/// Render the loading animation: centered `</>` logo with sweeping border accent.
///
/// The logo box is 7 chars wide and 3 chars tall (using rounded Unicode corners).
/// A 3-cell accent segment sweeps clockwise around the 16-position perimeter,
/// advancing one step per tick (250ms) for a 4-second revolution.
fn render_loading_animation(
    frame: &mut Frame,
    area: Rect,
    panel: &AgentPanel,
    theme: &Theme,
) {
    let box_width: u16 = 7;
    let box_height: u16 = 3;

    // Account for error message below logo.
    let total_height = if panel.loading_error.is_some() {
        box_height + 2
    } else {
        box_height
    };

    // Center the block in the area.
    if area.width < box_width || area.height < total_height {
        return;
    }
    let x = area.x + (area.width - box_width) / 2;
    let y = area.y + (area.height - total_height) / 2;

    // Perimeter: top(7) + right(1) + bottom(7) + left(1) = 16 cells.
    let perimeter: u16 = (box_width + box_height - 2) * 2;
    let accent_pos = panel.loading_tick % perimeter;
    let accent_len: u16 = 4;

    let is_accent = |p: u16| -> bool {
        for offset in 0..accent_len {
            if (accent_pos + offset) % perimeter == p {
                return true;
            }
        }
        false
    };

    let muted = Style::new().fg(theme.text_muted);
    let accent = Style::new().fg(theme.accent);
    let bold_text = Style::new().fg(theme.text).add_modifier(Modifier::BOLD);

    // Perimeter position mapping (clockwise starting top-left):
    //   Top:    positions 0..7       (left to right)
    //   Right:  position 7           (middle of right side)
    //   Bottom: positions 8..15      (right to left)
    //   Left:   position 15          (middle of left side)

    // ── Top row: ╭─────╮ ──
    for col in 0..box_width {
        let perim_pos = col;
        let style = if is_accent(perim_pos) { accent } else { muted };
        let ch = match col {
            0 => "\u{256d}",
            c if c == box_width - 1 => "\u{256e}",
            _ => "\u{2500}",
        };
        let span = Paragraph::new(ch).style(style);
        frame.render_widget(span, Rect::new(x + col, y, 1, 1));
    }

    // ── Middle row: │ </> │ ──
    let mid_y = y + 1;
    // Left border (perimeter position = perimeter - 1 = 15)
    let left_pos = perimeter - 1;
    let left_style = if is_accent(left_pos) { accent } else { muted };
    frame.render_widget(
        Paragraph::new("\u{2502}").style(left_style),
        Rect::new(x, mid_y, 1, 1),
    );
    // Logo text: " </> "
    frame.render_widget(
        Paragraph::new(" </> ").style(bold_text),
        Rect::new(x + 1, mid_y, 5, 1),
    );
    // Right border (perimeter position = box_width = 7)
    let right_pos = box_width;
    let right_style = if is_accent(right_pos) { accent } else { muted };
    frame.render_widget(
        Paragraph::new("\u{2502}").style(right_style),
        Rect::new(x + box_width - 1, mid_y, 1, 1),
    );

    // ── Bottom row: ╰─────╯ ── (right to left in perimeter)
    let bot_y = y + 2;
    for col in 0..box_width {
        // Bottom goes right-to-left: col 6→pos 8, col 5→pos 9, ..., col 0→pos 14
        let perim_pos = box_width + 1 + (box_width - 1 - col);
        let style = if is_accent(perim_pos) { accent } else { muted };
        let ch = match col {
            0 => "\u{2570}",
            c if c == box_width - 1 => "\u{256f}",
            _ => "\u{2500}",
        };
        let span = Paragraph::new(ch).style(style);
        frame.render_widget(span, Rect::new(x + col, bot_y, 1, 1));
    }

    // ── Error message below logo ──
    if let Some(ref error) = panel.loading_error {
        let error_y = bot_y + 2;
        if error_y < area.y + area.height {
            let max_width = area.width as usize;
            let truncated = if error.len() > max_width {
                format!("{}...", &error[..max_width.saturating_sub(3)])
            } else {
                error.clone()
            };
            let error_widget = Paragraph::new(truncated)
                .style(Style::new().fg(theme.error))
                .alignment(Alignment::Center);
            frame.render_widget(error_widget, Rect::new(area.x, error_y, area.width, 1));
        }
    }
}

/// Render the autocomplete popup above the input bar.
fn render_autocomplete(
    frame: &mut Frame,
    chat_area: Rect,
    input_area: Rect,
    input: &str,
    selected: Option<usize>,
    theme: &Theme,
) {
    let suggestions = autocomplete(input);
    if suggestions.is_empty() {
        return;
    }

    // Show all matching commands, capped by the available chat area height.
    let max_items = (chat_area.height.saturating_sub(2)) as usize; // -2 for borders
    let visible_count = suggestions.len().min(max_items);
    if visible_count == 0 {
        return;
    }

    let popup_height = visible_count as u16 + 2; // +2 for borders

    let popup_area = Rect {
        x: input_area.x,
        y: input_area.y.saturating_sub(popup_height),
        width: input_area.width.min(35),
        height: popup_height,
    };

    // Clear the area behind the popup.
    frame.render_widget(Clear, popup_area);

    let items: Vec<ListItem> = suggestions
        .iter()
        .take(visible_count)
        .enumerate()
        .map(|(i, s)| {
            let is_selected = selected == Some(i);
            let style = if is_selected {
                Style::new().fg(theme.selection_fg).bg(theme.selection_bg).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(theme.accent)
            };
            ListItem::new(Span::styled(s.as_str(), style))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(theme.accent))
            .title(" Commands "),
    );
    frame.render_widget(list, popup_area);
}

/// Render system messages as a centered popup overlay on top of panels.
fn render_system_popup(frame: &mut Frame, body_area: Rect, app: &App, theme: &Theme) {
    let text: String = app
        .system_messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    let lines: Vec<Line> = text.lines().map(|l| Line::from(l.to_string())).collect();
    let content_height = lines.len() as u16;

    // Size the popup to fit content, capped to 80% of body area.
    let max_w = (body_area.width * 4 / 5).max(40).min(body_area.width);
    let max_h = (body_area.height * 4 / 5).max(5).min(body_area.height);
    let popup_h = (content_height + 2).min(max_h); // +2 for borders
    let popup_w = max_w;

    // Center the popup in body_area.
    let x = body_area.x + (body_area.width.saturating_sub(popup_w)) / 2;
    let y = body_area.y + (body_area.height.saturating_sub(popup_h)) / 2;
    let popup_area = Rect::new(x, y, popup_w, popup_h);

    frame.render_widget(Clear, popup_area);

    let paragraph = Paragraph::new(lines)
        .style(Style::new().fg(theme.text).bg(theme.background))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(theme.accent))
                .style(Style::new().bg(theme.background)),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, popup_area);
}
