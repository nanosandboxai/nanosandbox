# Panel Zoom (Maximize/Minimize) Design

**Date:** 2026-03-09
**Branch:** feature/min-max-tui

## Problem

When running multiple agent panels side-by-side, each panel gets a fraction of the terminal width. Users need a way to temporarily expand one panel to full width for focused work, then restore the grid layout.

## Solution

Tmux-style zoom: toggle the focused panel between fullscreen (hiding other panels) and normal grid view.

## Approach: App-level `zoomed` boolean

A single `zoomed: bool` field on `App`. When true, the renderer shows only `panels[focused_panel]` at full body width. When false, normal grid layout.

### State

- `App.zoomed: bool` (default `false`)
- No changes to `AgentPanel`

### Triggers

- **Command:** `/max` or `/zoom` toggles `app.zoomed`
- **Keybinding:** `Ctrl+F` toggles `app.zoomed` (works in both Global and Panel input focus)

### Renderer

- `zoomed == true`: render only `panels[focused_panel]` using `Constraint::Percentage(100)` for the full body area
- `zoomed == false`: current grid logic (equal-width columns) unchanged
- Sidebars render normally alongside the zoomed panel

### Navigation while zoomed

- **Tab / Shift+Tab:** changes `focused_panel` as normal; renderer shows the newly focused panel fullscreen
- **Esc:** returns to Global input focus (does NOT unzoom)
- **/close or /kill:** closes panel; if no panels remain, `zoomed` resets to `false`

### Status bar

- When zoomed: show `Ctrl+F: restore` hint and `[1/N]` panel index indicator
- When not zoomed: show `Ctrl+F: maximize` hint

### Edge cases

- **Single panel:** zoom toggles state but no visible change
- **Panel added while zoomed:** stays zoomed on current panel; new panel reachable via Tab
- **All panels closed while zoomed:** `zoomed` resets to `false`

## Alternatives considered

1. **Per-panel ZoomState enum:** each panel owns a `Normal | Maximized` state. More flexible but adds complexity to enforce single-maximized invariant and transfer state on Tab.
2. **Layout manager abstraction:** `LayoutMode` enum between app and renderer. Clean separation but over-engineered for a binary toggle.

Both rejected in favor of simplicity (YAGNI).
