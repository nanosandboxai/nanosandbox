//! Renderer that draws the TUI frames.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, AgentPanel, InputFocus, MessageRole, PanelMode};
use super::commands::autocomplete;
use super::grid::grid_dimensions;

/// Maximum number of visual lines the input area can grow to.
const MAX_INPUT_HEIGHT: u16 = 10;

/// Render a full TUI frame based on the current application state.
pub fn render(frame: &mut Frame, app: &mut App) {
    // Compute dynamic global input height.
    let global_content_width = (frame.area().width as usize).saturating_sub(3).max(1);
    let global_input_height = if app.input_focus == InputFocus::Global {
        (app.global_input.visual_line_count(global_content_width) as u16)
            .max(1)
            .min(MAX_INPUT_HEIGHT)
    } else {
        1
    };

    let show_header = app.panels.is_empty();

    let (body_area, global_input_area, status_area) = if show_header {
        let [header_area, body_area, global_input_area, status_area] = Layout::vertical([
            Constraint::Length(4),
            Constraint::Fill(1),
            Constraint::Length(global_input_height),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        render_header(frame, header_area);
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

    if app.panels.is_empty() {
        render_welcome(frame, body_area, app);
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
            render_mcp_sidebar(frame, sidebar_area);
        }
    } else {
        render_panel_grid(frame, body_area, app);
    }

    // Render autocomplete popup for the global input bar (overlays on top of body).
    if app.input_focus == InputFocus::Global && app.global_input.text().starts_with('/') {
        render_autocomplete(frame, body_area, global_input_area, app.global_input.text(), app.autocomplete_index);
    }
}

/// Render the header with an ASCII logo (dashed box with sparkles and </>).
fn render_header(frame: &mut Frame, area: Rect) {
    let b = Style::new().fg(Color::DarkGray);
    let s = Style::new().fg(Color::Cyan);
    let w = Style::new().fg(Color::White).add_modifier(Modifier::BOLD);
    let header = Paragraph::new(vec![
        Line::from(vec![Span::styled("+-------+", b)]),
        Line::from(vec![
            Span::styled("| ", b),
            Span::styled("\u{2726}", s),
            Span::styled(" ", b),
            Span::styled("\u{2726}", s),
            Span::styled(" ", b),
            Span::styled("\u{2726}", s),
            Span::styled(" |", b),
        ]),
        Line::from(vec![
            Span::styled("|  ", b),
            Span::styled("</>", w),
            Span::styled("  |", b),
        ]),
        Line::from(vec![Span::styled("+-------+", b)]),
    ])
    .alignment(Alignment::Center);
    frame.render_widget(header, area);
}

/// Render the persistent global input bar with multiline wrapping and real cursor.
fn render_global_input(frame: &mut Frame, area: Rect, app: &mut App) {
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
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    let text_style = if is_focused {
        Style::default()
    } else {
        Style::new().fg(Color::DarkGray)
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
        lines.push(Line::from(Span::styled(prompt, prompt_style)));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);

    // Place real terminal cursor when focused.
    if is_focused {
        let (cursor_row, cursor_col) = app.global_input.cursor_visual_position(content_width);
        let visual_row = cursor_row.saturating_sub(scroll_offset);
        let x = area.x + prompt_len + (cursor_col as u16).min(area.width.saturating_sub(prompt_len + 1));
        let y = area.y + visual_row as u16;
        if y < area.y + area.height {
            frame.set_cursor_position((x, y));
        }
    }
}

/// Render the status bar with keybinding hints.
fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let hints = if app.panels.is_empty() {
        Line::from(vec![
            Span::styled(" /add <agent>", Style::new().fg(Color::Cyan)),
            Span::raw(" new panel  "),
            Span::styled("/quit", Style::new().fg(Color::Cyan)),
            Span::raw(" exit"),
        ])
    } else if app.input_focus == InputFocus::Global {
        let mut spans = vec![
            Span::styled(" Tab", Style::new().fg(Color::Cyan)),
            Span::raw(" panel focus  "),
            Span::styled("/kill", Style::new().fg(Color::Cyan)),
            Span::raw(" destroy  "),
            Span::styled("/sb", Style::new().fg(Color::Cyan)),
            Span::raw(" sandboxes  "),
            Span::styled("/add", Style::new().fg(Color::Cyan)),
            Span::raw(" new  "),
            Span::styled("/quit", Style::new().fg(Color::Cyan)),
            Span::raw(" exit  "),
            Span::styled("^F", Style::new().fg(Color::Cyan)),
            Span::raw(if app.zoomed { " restore" } else { " maximize" }),
        ];
        if app.zoomed {
            spans.push(Span::styled(
                format!("  [{}/{}]", app.focused_panel, app.panels.len()),
                Style::new().fg(Color::Yellow),
            ));
        }
        Line::from(spans)
    } else {
        // Check if focused panel is in terminal mode.
        let in_terminal = app
            .panels
            .get(app.focused_panel)
            .map_or(false, |p| p.mode == PanelMode::Terminal);

        if in_terminal {
            let mut spans = vec![
                Span::styled(" Esc", Style::new().fg(Color::Cyan)),
                Span::raw(" global bar  "),
                Span::styled("Tab", Style::new().fg(Color::Cyan)),
                Span::raw(" next panel  "),
                Span::styled("SSH Terminal", Style::new().fg(Color::Green)),
                Span::raw("  "),
                Span::styled("^F", Style::new().fg(Color::Cyan)),
                Span::raw(if app.zoomed { " restore" } else { " maximize" }),
            ];
            if app.zoomed {
                spans.push(Span::styled(
                    format!("  [{}/{}]", app.focused_panel, app.panels.len()),
                    Style::new().fg(Color::Yellow),
                ));
            }
            Line::from(spans)
        } else {
            let mut spans = vec![
                Span::styled(" Esc", Style::new().fg(Color::Cyan)),
                Span::raw(" global bar  "),
                Span::styled("Tab", Style::new().fg(Color::Cyan)),
                Span::raw(" next panel  "),
                Span::styled("Shift+Enter", Style::new().fg(Color::Cyan)),
                Span::raw(" newline  "),
                Span::styled("^F", Style::new().fg(Color::Cyan)),
                Span::raw(if app.zoomed { " restore" } else { " maximize" }),
            ];
            if app.zoomed {
                spans.push(Span::styled(
                    format!("  [{}/{}]", app.focused_panel, app.panels.len()),
                    Style::new().fg(Color::Yellow),
                ));
            }
            Line::from(spans)
        }
    };

    let bar = Paragraph::new(hints).style(Style::new().bg(Color::DarkGray));
    frame.render_widget(bar, area);
}

/// Render the welcome screen shown when no panels exist.
/// The global input bar is rendered separately by `render()`.
fn render_welcome(frame: &mut Frame, area: Rect, app: &App) {
    if app.system_messages.is_empty() {
        let lines = vec![
            Line::from(""),
            Line::from("No agent panels are open."),
            Line::from(""),
            Line::from(vec![
                Span::raw("Type "),
                Span::styled("/add <agent>", Style::new().fg(Color::Green)),
                Span::raw(" to spawn a new sandbox panel."),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw("Examples: "),
                Span::styled("/add claude", Style::new().fg(Color::Yellow)),
                Span::raw("  "),
                Span::styled("/add codex", Style::new().fg(Color::Yellow)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw("Type "),
                Span::styled("/help", Style::new().fg(Color::Green)),
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
                let style = Style::new().fg(Color::Yellow);
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
fn render_mcp_sidebar(frame: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(Span::styled(
            "MCP Servers",
            Style::new()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("/mcp list", Style::new().fg(Color::Green)),
            Span::raw("  list servers"),
        ]),
        Line::from(vec![
            Span::styled("/mcp add", Style::new().fg(Color::Green)),
            Span::raw("   add server"),
        ]),
        Line::from(vec![
            Span::styled("/mcp remove", Style::new().fg(Color::Green)),
            Span::raw(" remove server"),
        ]),
        Line::from(vec![
            Span::styled("/mcp enable", Style::new().fg(Color::Green)),
            Span::raw(" enable server"),
        ]),
        Line::from(vec![
            Span::styled("/mcp disable", Style::new().fg(Color::Green)),
            Span::raw(" disable"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Press /mcp to toggle this sidebar.",
            Style::new().fg(Color::DarkGray),
        )),
    ];

    let sidebar = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(Color::DarkGray))
                .title(" MCP "),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(sidebar, area);
}

/// Render the sandbox sidebar listing all running panels.
fn render_sandbox_sidebar(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines = vec![
        Line::from(Span::styled(
            "Sandboxes",
            Style::new()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    if app.panels.is_empty() {
        lines.push(Line::from(Span::styled(
            "No sandboxes running.",
            Style::new().fg(Color::DarkGray),
        )));
    } else {
        for (i, panel) in app.panels.iter().enumerate() {
            let idx = i;
            let is_focused = i == app.focused_panel;

            let status = if panel.mode == PanelMode::Terminal {
                Span::styled("● ", Style::new().fg(Color::Green))
            } else if panel.sandbox.is_some() {
                Span::styled("◌ ", Style::new().fg(Color::Yellow))
            } else {
                Span::styled("○ ", Style::new().fg(Color::DarkGray))
            };

            let name_style = if is_focused {
                Style::new().fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(Color::White)
            };

            let sid = if panel.sandbox_id_short.is_empty() {
                String::new()
            } else {
                format!(" {}", panel.sandbox_id_short)
            };

            let focus_marker = if is_focused { " *" } else { "" };

            lines.push(Line::from(vec![
                Span::raw(format!(" [{}] ", idx)),
                status,
                Span::styled(&panel.agent_name, name_style),
                Span::styled(sid, Style::new().fg(Color::DarkGray)),
                Span::styled(focus_marker, Style::new().fg(Color::Cyan)),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Press /sb to toggle this sidebar.",
        Style::new().fg(Color::DarkGray),
    )));

    let sidebar = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(Color::DarkGray))
                .title(" Sandboxes "),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(sidebar, area);
}

/// Render the panel grid based on the number of panels.
fn render_panel_grid(frame: &mut Frame, area: Rect, app: &mut App) {
    let panel_count = app.panels.len();
    if panel_count == 0 {
        return;
    }

    // Zoomed mode: render only the focused panel at full width.
    if app.zoomed {
        let idx = app.focused_panel;
        if idx < panel_count {
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
            );
        }
        return;
    }

    let (rows, cols) = grid_dimensions(panel_count);
    if rows == 0 || cols == 0 {
        return;
    }

    // Split vertically into rows.
    let row_constraints: Vec<Constraint> = (0..rows)
        .map(|_| Constraint::Ratio(1, rows as u32))
        .collect();
    let row_areas = Layout::vertical(row_constraints).split(area);

    let mut panel_idx = 0;

    for row in 0..rows {
        if panel_idx >= panel_count {
            break;
        }

        // Determine how many columns this row actually has.
        let cols_in_row = cols.min(panel_count - panel_idx);

        let col_constraints: Vec<Constraint> = (0..cols_in_row)
            .map(|_| Constraint::Ratio(1, cols_in_row as u32))
            .collect();
        let col_areas = Layout::horizontal(col_constraints).split(row_areas[row]);

        for col in 0..cols_in_row {
            if panel_idx < panel_count {
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
                );
                panel_idx += 1;
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
    is_input_focused: bool,
    autocomplete_index: Option<usize>,
) {
    // Status indicator.
    let status_indicator = if panel.is_streaming {
        Span::styled("◌ ", Style::new().fg(Color::Yellow))
    } else if panel.sandbox.is_some() {
        Span::styled("● ", Style::new().fg(Color::Green))
    } else {
        Span::styled("○ ", Style::new().fg(Color::DarkGray))
    };

    // Build title line.
    let sandbox_label = if panel.sandbox_id_short.is_empty() {
        String::new()
    } else {
        format!(" {}", panel.sandbox_id_short)
    };

    let mode_label = match panel.mode {
        PanelMode::Agent => "",
        PanelMode::Terminal => "",
    };

    let env_indicator = if panel.env.is_empty() {
        ""
    } else {
        " [env]"
    };

    let title = Line::from(vec![
        Span::raw(" "),
        status_indicator,
        Span::styled(
            &panel.agent_name,
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            mode_label,
            Style::new()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(env_indicator, Style::new().fg(Color::Green)),
        Span::styled(
            format!(" [{}]", index),
            Style::new().fg(Color::DarkGray),
        ),
        Span::styled(sandbox_label, Style::new().fg(Color::DarkGray)),
        Span::raw(" "),
    ]);

    let border_color = if is_focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(border_color))
        .title(title);

    let inner_area = block.inner(area);
    frame.render_widget(block, area);

    if inner_area.height < 2 || inner_area.width < 2 {
        return;
    }

    // Terminal mode: render PseudoTerminal widget for the entire inner area.
    if panel.mode == PanelMode::Terminal {
        let cols = inner_area.width;
        let rows = inner_area.height;

        // Resize detection: update vt100 parser and notify SSH channel.
        if (cols, rows) != panel.last_terminal_size {
            panel.last_terminal_size = (cols, rows);
            if let Some(ref mut term) = panel.terminal {
                term.resize(cols, rows);
            }
            if let Some(ref handle) = panel.terminal_handle {
                let _ = handle.resize_tx.send((cols, rows));
            }
        }

        // Render the terminal screen.
        if let Some(ref term) = panel.terminal {
            let pseudo_term = tui_term::widget::PseudoTerminal::new(term.screen());
            frame.render_widget(pseudo_term, inner_area);

            // Place cursor at the terminal's cursor position when focused.
            if is_focused {
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

    // Compute dynamic input height.
    let prompt_len: u16 = 2; // "> " or "$ "
    let content_width = (inner_area.width.saturating_sub(prompt_len) as usize).max(1);

    // Cache width for key handler.
    panel.last_input_width = content_width as u16;

    let input_height = if is_input_focused {
        (panel.input.visual_line_count(content_width) as u16)
            .max(1)
            .min(MAX_INPUT_HEIGHT.min(inner_area.height.saturating_sub(1)))
    } else {
        1
    };

    // Split inner area into chat area and input bar.
    let [chat_area, input_area] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(input_height),
    ])
    .areas(inner_area);

    // Render chat messages.
    render_chat(frame, chat_area, panel);

    // Render input bar — cursor only shows when this panel's input is focused.
    render_input(frame, input_area, panel, is_input_focused);

    // Render autocomplete popup if panel input is focused.
    if is_input_focused && panel.input.text().starts_with('/') {
        render_autocomplete(frame, chat_area, input_area, panel.input.text(), autocomplete_index);
    }
}

/// Render the chat message area for a panel.
fn render_chat(frame: &mut Frame, area: Rect, panel: &AgentPanel) {
    let lines: Vec<Line> = panel
        .chat_history
        .iter()
        .flat_map(|msg| {
            let (prefix, style) = match msg.role {
                MessageRole::User => (
                    "You: ",
                    Style::new().fg(Color::Green),
                ),
                MessageRole::Agent => (
                    "",
                    Style::new().fg(Color::White),
                ),
                MessageRole::System => (
                    "! ",
                    Style::new().fg(Color::Yellow),
                ),
            };

            // Split content into lines, preserving multi-line messages.
            msg.content
                .lines()
                .enumerate()
                .map(|(i, line_text)| {
                    if i == 0 && !prefix.is_empty() {
                        Line::from(vec![
                            Span::styled(prefix, style.add_modifier(Modifier::BOLD)),
                            Span::styled(line_text, style),
                        ])
                    } else {
                        Line::from(Span::styled(line_text, style))
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect();

    let paragraph = Paragraph::new(lines)
        .scroll((panel.scroll_offset, 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

/// Render the input bar at the bottom of a panel with multiline wrapping and real cursor.
fn render_input(frame: &mut Frame, area: Rect, panel: &AgentPanel, is_focused: bool) {
    let prompt = match panel.mode {
        PanelMode::Agent => "> ",
        PanelMode::Terminal => "> ",
    };

    let prompt_len = prompt.len() as u16;
    let content_width = (area.width.saturating_sub(prompt_len) as usize).max(1);

    let visual_lines = panel.input.visual_lines(content_width);

    // Compute scroll offset if content exceeds area height.
    let viewport_height = area.height as usize;
    let scroll_offset = if is_focused {
        let (cursor_row, _) = panel.input.cursor_visual_position(content_width);
        if cursor_row >= viewport_height {
            cursor_row - viewport_height + 1
        } else {
            0
        }
    } else {
        0
    };

    let prompt_style = if is_focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    };

    let mut lines: Vec<Line> = Vec::new();
    for (i, vl) in visual_lines.iter().enumerate().skip(scroll_offset).take(viewport_height) {
        let text_slice = &panel.input.text()[vl.byte_start..vl.byte_end];
        let prefix = if i == 0 {
            Span::styled(prompt, prompt_style)
        } else {
            Span::styled("  ", prompt_style)
        };
        lines.push(Line::from(vec![prefix, Span::raw(text_slice)]));
    }

    // Handle empty input.
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(prompt, prompt_style)));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);

    // Place real terminal cursor when focused.
    if is_focused {
        let (cursor_row, cursor_col) = panel.input.cursor_visual_position(content_width);
        let visual_row = cursor_row.saturating_sub(scroll_offset);
        let x = area.x + prompt_len + (cursor_col as u16).min(area.width.saturating_sub(prompt_len + 1));
        let y = area.y + visual_row as u16;
        if y < area.y + area.height {
            frame.set_cursor_position((x, y));
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
                Style::new().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(Color::Cyan)
            };
            ListItem::new(Span::styled(s.as_str(), style))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(Color::Cyan))
            .title(" Commands "),
    );
    frame.render_widget(list, popup_area);
}
