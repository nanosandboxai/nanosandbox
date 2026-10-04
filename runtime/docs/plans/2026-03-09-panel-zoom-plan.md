# Panel Zoom Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add tmux-style panel zoom — toggle the focused panel between fullscreen and normal grid view.

**Architecture:** Single `zoomed: bool` on `App`. When true, renderer shows only the focused panel at full width. Ctrl+F keybinding + `/max` command toggle it. Tab cycles panels while staying zoomed.

**Tech Stack:** Rust, ratatui 0.29, crossterm

---

### Task 1: Add `zoomed` field to App + unit tests

**Files:**
- Modify: `src/tui/app.rs:126-173` (App struct + new())
- Test: `src/tui/app.rs:475+` (tests module)

**Step 1: Write the failing test**

Add to the test module at the bottom of `src/tui/app.rs`:

```rust
#[test]
fn test_zoomed_default_false() {
    let app = App::new();
    assert!(!app.zoomed);
}

#[test]
fn test_toggle_zoom() {
    let mut app = App::new();
    app.panels.push(AgentPanel::new("test"));
    app.zoomed = !app.zoomed;
    assert!(app.zoomed);
    app.zoomed = !app.zoomed;
    assert!(!app.zoomed);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --features cli test_zoomed_default_false -- --nocapture 2>&1`
Expected: FAIL — `zoomed` field doesn't exist on `App`

**Step 3: Write minimal implementation**

Add `pub zoomed: bool` to the `App` struct (after `show_sandbox_sidebar` on line 136):

```rust
    /// Whether a panel is zoomed to full width.
    pub zoomed: bool,
```

Add `zoomed: false,` to `App::new()` (after `show_sandbox_sidebar: false,` on line 166):

```rust
            zoomed: false,
```

**Step 4: Run test to verify it passes**

Run: `cargo test --features cli test_zoomed -- --nocapture 2>&1`
Expected: PASS (both `test_zoomed_default_false` and `test_toggle_zoom`)

**Step 5: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat(tui): add zoomed field to App state"
```

---

### Task 2: Add `/max` and `/zoom` commands + unit tests

**Files:**
- Modify: `src/tui/commands.rs:4-70` (Command enum)
- Modify: `src/tui/commands.rs:86-91` (ALL_COMMANDS)
- Modify: `src/tui/commands.rs:116-136` (parse_command_verbose match)
- Test: `src/tui/commands.rs:329+` (tests module)

**Step 1: Write the failing tests**

Add to the test module at the bottom of `src/tui/commands.rs`:

```rust
#[test]
fn test_parse_max() {
    assert_eq!(parse_command("/max"), Some(Command::Zoom));
}

#[test]
fn test_parse_zoom() {
    assert_eq!(parse_command("/zoom"), Some(Command::Zoom));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --features cli test_parse_max -- --nocapture 2>&1`
Expected: FAIL — no `Zoom` variant on `Command`

**Step 3: Write minimal implementation**

Add variant to `Command` enum (after `Copy` on line 69):

```rust
    /// Toggle zoom (maximize/minimize) for the focused panel.
    Zoom,
```

Add to `ALL_COMMANDS` array (after `"/copy"` on line 87):

```rust
    "/max", "/zoom",
```

Add match arms in `parse_command_verbose` (after the `"/copy"` arm on line 129):

```rust
        "/max" | "/zoom" => ParseResult::Ok(Command::Zoom),
```

**Step 4: Run test to verify it passes**

Run: `cargo test --features cli test_parse_max test_parse_zoom -- --nocapture 2>&1`
Expected: PASS

**Step 5: Commit**

```bash
git add src/tui/commands.rs
git commit -m "feat(tui): add /max and /zoom commands"
```

---

### Task 3: Handle `/max` command in run.rs + Ctrl+F keybinding

**Files:**
- Modify: `src/tui/run.rs:356-564` (handle_key_event — add Ctrl+F)
- Modify: `src/tui/run.rs:567-807` (handle_command — add Zoom arm)
- Modify: `src/tui/run.rs:586-614` (Help command text — add /max)

**Step 1: Add Ctrl+F keybinding to handle_key_event**

In `handle_key_event`, in the terminal mode section (line 365), add a Ctrl+F intercept before the catch-all `_` arm:

```rust
                    // Ctrl+F: toggle zoom
                    KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if !app.panels.is_empty() {
                            app.zoomed = !app.zoomed;
                        }
                        return;
                    }
```

In the main `match key.code` block (line 400), add before the `KeyCode::Char(c)` arm (line 502):

```rust
        // Ctrl+F: toggle zoom.
        KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if !app.panels.is_empty() {
                app.zoomed = !app.zoomed;
            }
        }
```

**Step 2: Add Command::Zoom handler in handle_command**

Add a new arm in `handle_command` (after the `Command::Copy` arm on line 803):

```rust
        Command::Zoom => {
            if !app.panels.is_empty() {
                app.zoomed = !app.zoomed;
            }
        }
```

**Step 3: Update help text**

In the `Command::Help` arm (line 586), add after the `/copy` line:

```rust
                    "  /max (/zoom)                  Toggle panel zoom\n",
```

**Step 4: Reset zoom when all panels removed**

In `Command::Close` handler (line 623), after `app.focus_global();` on line 629, add:

```rust
                    app.zoomed = false;
```

In `Command::Kill` handler (line 724), after `app.focus_global();` on line 748, add:

```rust
                    app.zoomed = false;
```

**Step 5: Build to verify compilation**

Run: `cargo build --features cli 2>&1`
Expected: compiles successfully

**Step 6: Commit**

```bash
git add src/tui/run.rs
git commit -m "feat(tui): handle /max command and Ctrl+F zoom toggle"
```

---

### Task 4: Render zoomed panel in renderer.rs

**Files:**
- Modify: `src/tui/renderer.rs:397-451` (render_panel_grid)

**Step 1: Add zoom logic to render_panel_grid**

Replace the `render_panel_grid` function body. At the start of the function (after the early returns on lines 399-406), add a zoomed branch before the grid logic:

```rust
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
```

**Step 2: Build to verify compilation**

Run: `cargo build --features cli 2>&1`
Expected: compiles successfully

**Step 3: Commit**

```bash
git add src/tui/renderer.rs
git commit -m "feat(tui): render only focused panel when zoomed"
```

---

### Task 5: Update status bar hints for zoom state

**Files:**
- Modify: `src/tui/renderer.rs:173-223` (render_status_bar)

**Step 1: Add zoom hints to status bar**

In `render_status_bar`, update the hints for the case when panels exist and input_focus is Global (line 181). Add zoom hint at the end:

For the `InputFocus::Global` branch (line 181), append before the closing `])`:

```rust
            Span::styled("  ^F", Style::new().fg(Color::Cyan)),
            Span::raw(if app.zoomed { " restore" } else { " maximize" }),
```

For the panel-focused branches (terminal mode line 201, and agent mode line 209), append zoom hint spans:

Terminal mode (after "SSH Terminal" span):
```rust
                Span::raw("  "),
                Span::styled("^F", Style::new().fg(Color::Cyan)),
                Span::raw(if app.zoomed { " restore" } else { " maximize" }),
```

Agent mode (after "newline" span):
```rust
                Span::styled("^F", Style::new().fg(Color::Cyan)),
                Span::raw(if app.zoomed { " restore" } else { " maximize" }),
```

When zoomed, also add a panel index indicator. In the `render_status_bar` function, add after computing `hints` and before creating the `bar` paragraph:

Add a `[N/M]` indicator when zoomed by prepending to the hints line. The simplest approach: wrap the hints construction so that when zoomed, the first span shows the index:

Replace the beginning of the global-input focused hints (line 182) with:

```rust
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
                format!("  [{}/{}]", app.focused_panel + 1, app.panels.len()),
                Style::new().fg(Color::Yellow),
            ));
        }
        Line::from(spans)
```

For the panel-focused terminal and agent branches, follow the same pattern — append zoom hint spans and optional `[N/M]` when zoomed.

**Step 2: Build to verify compilation**

Run: `cargo build --features cli 2>&1`
Expected: compiles successfully

**Step 3: Commit**

```bash
git add src/tui/renderer.rs
git commit -m "feat(tui): show zoom hints and panel index in status bar"
```

---

### Task 6: Run full test suite

**Step 1: Run all tests**

Run: `cargo test --features cli 2>&1`
Expected: all tests pass (unit + integration)

**Step 2: Run clippy**

Run: `cargo clippy --features cli 2>&1`
Expected: no warnings

**Step 3: Fix any issues**

If tests fail or clippy warns, fix the issues and re-run.

**Step 4: Final commit if any fixes needed**

```bash
git add -A
git commit -m "fix(tui): address test/clippy issues from zoom feature"
```
