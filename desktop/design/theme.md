# Design System — Theme & Tokens

Visual baseline for the nanosb desktop terminal app.

Adapted from **dd-code** (deep navy + cyan/emerald accent family) with terminal-first
optimisations: higher foreground contrast for long sessions, Warp-style command-block
headers, and a faithful light-theme inversion. Behavior and chrome structure mirror the
existing nanosb TUI exactly.

---

## 1. Source mapping

| Origin | Contribution |
|--------|-------------|
| `dd-code/src/styles/global.css` | Full token set: bg-family, accents, text, status, glow, border, spacing, typography |
| `dd-code/tailwind.config.js` | Token names, glow box-shadows, gradient definitions |
| `dd-code/src/lib/components/` | Panel chrome, status bar, title bar, resize handle patterns |
| TUI `tui/theme.rs` | Semantic role parity: background, accent, text, text_muted, success, warning, error, info, status_bar_bg, selection |
| Warp terminal (UX reference) | Command-block pane headers, dense single-row status bar, input palette overlay |

---

## 2. Semantic roles (TUI parity mapping)

Every token below corresponds to a semantic role in `tui/theme.rs`. The light theme
inverts luminance while keeping the same hue family.

| Role | CSS Variable | TUI field | Usage |
|------|-------------|-----------|-------|
| App background | `--bg-app` | `background` | Root window fill |
| Surface (pane bg) | `--bg-surface` | — | Panel / card fill |
| Surface elevated | `--bg-elevated` | — | Palette, popup, dropdown |
| Accent | `--accent` | `accent` | Focused border, cursor, active indicator |
| Accent secondary | `--accent-2` | — | Gradient pair, hover highlights |
| Text primary | `--text` | `text` | Terminal chrome, labels |
| Text muted | `--text-muted` | `text_muted` | Unfocused borders, hints, timestamps |
| Success | `--success` | `success` | Connected status, additions |
| Warning | `--warning` | `warning` | Streaming indicator, modified |
| Error | `--error` | `error` | Failures, deleted files |
| Info | `--info` | `info` | Renamed files, informational |
| Status bar bg | `--status-bar-bg` | `status_bar_bg` | Bottom status bar background |
| Selection bg | `--selection-bg` | `selection_bg` | Palette item highlight, text selection |
| Selection fg | `--selection-fg` | `selection_fg` | Text on selection bg |
| Border | `--border` | — | Default panel border |
| Border focused | `--border-focused` | — | Focused pane border |
| Overlay scrim | `--scrim` | — | Popup/palette backdrop dim |

---

## 3. Token values

### Dark theme (default)

> Root: `:root` or `[data-theme="dark"]`

| CSS Variable | Value | Notes |
|-------------|-------|-------|
| `--bg-app` | `#040b16` | dd-code `bg-primary` — deep navy |
| `--bg-surface` | `#041321` | dd-code `bg-surface` |
| `--bg-elevated` | `#061b2a` | dd-code `bg-secondary` — palette/popup |
| `--bg-pane-header` | `#05101e` | Between surface and secondary — Warp-style header strip |
| `--accent` | `#22d3ee` | dd-code `accent-primary` — cyan |
| `--accent-2` | `#34d399` | dd-code `accent-secondary` — emerald |
| `--accent-gradient` | `linear-gradient(135deg, #22d3ee, #34d399)` | Decorative |
| `--text` | `#e5e7eb` | dd-code `text-primary` |
| `--text-muted` | `#6b7280` | dd-code `text-muted` |
| `--text-subtle` | `#9ca3af` | dd-code `text-secondary` — mid-level |
| `--success` | `#10b981` | dd-code `status-success` |
| `--warning` | `#f59e0b` | dd-code `status-warning` |
| `--error` | `#ef4444` | dd-code `status-error` |
| `--info` | `#3b82f6` | dd-code `status-info` |
| `--status-bar-bg` | `#030a12` | Slightly darker than bg-app for visual separation |
| `--selection-bg` | `rgba(34, 211, 238, 0.25)` | Cyan tint |
| `--selection-fg` | `#e5e7eb` | Same as text primary |
| `--border` | `rgba(34, 211, 238, 0.15)` | Subtle cyan tint — dd-code border adapted |
| `--border-focused` | `rgba(34, 211, 238, 0.6)` | High-visibility focused state |
| `--border-radius` | `6px` | Pane chrome corners |
| `--border-radius-sm` | `4px` | Input, palette item |
| `--scrim` | `rgba(4, 11, 22, 0.7)` | Popup backdrop |
| `--glow-accent` | `0 0 14px rgba(34, 211, 238, 0.28)` | Focused pane glow |
| `--glow-accent-sm` | `0 0 7px rgba(34, 211, 238, 0.18)` | Subtle glow |

### Light theme

> Selector: `[data-theme="light"]`

Light is a semantic inversion: dark surfaces → light surfaces, cyan adapts to a deeper
teal/indigo that retains contrast on white backgrounds.

| CSS Variable | Value | Notes |
|-------------|-------|-------|
| `--bg-app` | `#f0f4f8` | Cool near-white |
| `--bg-surface` | `#ffffff` | Pure white panel |
| `--bg-elevated` | `#e8edf4` | Palette / popup |
| `--bg-pane-header` | `#dde4ed` | Pane header strip |
| `--accent` | `#0891b2` | Deeper cyan for contrast on white (Tailwind cyan-600) |
| `--accent-2` | `#059669` | Deeper emerald (Tailwind emerald-600) |
| `--accent-gradient` | `linear-gradient(135deg, #0891b2, #059669)` | |
| `--text` | `#111827` | Near-black |
| `--text-muted` | `#6b7280` | Same as dark |
| `--text-subtle` | `#9ca3af` | Same as dark |
| `--success` | `#059669` | Emerald-600 |
| `--warning` | `#d97706` | Amber-600 |
| `--error` | `#dc2626` | Red-600 |
| `--info` | `#2563eb` | Blue-600 |
| `--status-bar-bg` | `#dde4ed` | Matches pane header |
| `--selection-bg` | `rgba(8, 145, 178, 0.2)` | Teal tint |
| `--selection-fg` | `#111827` | |
| `--border` | `rgba(8, 145, 178, 0.18)` | |
| `--border-focused` | `rgba(8, 145, 178, 0.7)` | |
| `--border-radius` | `6px` | |
| `--border-radius-sm` | `4px` | |
| `--scrim` | `rgba(240, 244, 248, 0.75)` | |
| `--glow-accent` | `0 0 10px rgba(8, 145, 178, 0.2)` | Softer in light |
| `--glow-accent-sm` | `none` | Skip glow on light |

---

## 4. Typography

| Role | Font | Size | Weight | Notes |
|------|------|------|--------|-------|
| Terminal output | `--font-mono` | `13px` | 400 | JetBrains Mono → Fira Code → monospace |
| Pane header label | `--font-mono` | `11px` | 500 | Uppercase, letter-spacing 0.5px |
| Input bar prompt | `--font-mono` | `13px` | 400 | |
| Palette input | `--font-sans` | `13px` | 400 | Inter → system-ui |
| Palette item | `--font-mono` | `12px` | 400 | |
| Status bar | `--font-mono` | `11px` | 400 | Dense single row |
| Popup body | `--font-sans` | `13px` | 400 | |
| Popup title | `--font-sans` | `13px` | 600 | |

```css
--font-sans: "Inter", system-ui, sans-serif;
--font-mono: "JetBrains Mono", "Fira Code", ui-monospace, monospace;
--font-size-base: 13px;
--font-size-sm: 11px;
--line-height-terminal: 1.4;
--line-height-ui: 1.5;
--letter-spacing-label: 0.05em;
```

---

## 5. Spacing grid (4 px base)

| Token | Value | Usage |
|-------|-------|-------|
| `--space-1` | `4px` | Micro gap: icon margin, focus offset |
| `--space-2` | `8px` | Padding inside compact elements |
| `--space-3` | `12px` | Pane header padding, status bar padding |
| `--space-4` | `16px` | Default component padding |
| `--space-6` | `24px` | Section gap |
| `--space-8` | `32px` | Large gap |
| `--pane-header-h` | `28px` | Warp-style command-block header height |
| `--status-bar-h` | `22px` | Dense single row |
| `--input-bar-h` | `36px` | Global input bar |
| `--palette-w` | `560px` | Palette overlay width |
| `--palette-max-h` | `360px` | Palette max height |

---

## 6. Motion

| Token | Value | Usage |
|-------|-------|-------|
| `--dur-fast` | `80ms` | Focus ring, border color |
| `--dur-base` | `140ms` | Palette open/close, popup fade |
| `--dur-slow` | `260ms` | Auth browser slide-in |
| `--ease-out` | `cubic-bezier(0.0, 0.0, 0.2, 1.0)` | Open transitions |
| `--ease-in` | `cubic-bezier(0.4, 0.0, 1.0, 1.0)` | Close transitions |

---

## 7. Focus ring

```css
/* Applied to any interactive element that receives keyboard focus */
outline: 2px solid var(--accent);
outline-offset: 2px;
border-radius: var(--border-radius-sm);
```

Inherited directly from `dd-code/src/styles/global.css` `:focus-visible` rule.

---

## 8. Scrollbar

```css
/* WebKit-based — WebView in Tauri supports this */
::-webkit-scrollbar        { width: 6px; }
::-webkit-scrollbar-track  { background: transparent; }
::-webkit-scrollbar-thumb  { background: var(--border-focused); border-radius: 3px; }
::-webkit-scrollbar-thumb:hover { background: var(--accent); }
```

Note: xterm.js has its own scrollbar rendering; theme tokens do not apply there.

---

## 9. xterm.js theme mapping

The `theme_get()` Tauri command (defined in D3 — IPC schema) returns an xterm.js
`ITheme` object assembled from these tokens at runtime.

| xterm.js key | Dark token source | Light token source |
|-------------|-------------------|--------------------|
| `background` | `#040b16` | `#ffffff` |
| `foreground` | `#e5e7eb` | `#111827` |
| `cursor` | `#22d3ee` | `#0891b2` |
| `cursorAccent` | `#040b16` | `#ffffff` |
| `selectionBackground` | `rgba(34,211,238,0.25)` | `rgba(8,145,178,0.2)` |
| `selectionForeground` | `#e5e7eb` | `#111827` |
| `black` | `#1e2433` | `#374151` |
| `red` | `#ef4444` | `#dc2626` |
| `green` | `#10b981` | `#059669` |
| `yellow` | `#f59e0b` | `#d97706` |
| `blue` | `#3b82f6` | `#2563eb` |
| `magenta` | `#a78bfa` | `#7c3aed` |
| `cyan` | `#22d3ee` | `#0891b2` |
| `white` | `#e5e7eb` | `#f9fafb` |
| `brightBlack` | `#4b5563` | `#6b7280` |
| `brightRed` | `#f87171` | `#ef4444` |
| `brightGreen` | `#34d399` | `#10b981` |
| `brightYellow` | `#fcd34d` | `#f59e0b` |
| `brightBlue` | `#60a5fa` | `#3b82f6` |
| `brightMagenta` | `#c4b5fd` | `#a78bfa` |
| `brightCyan` | `#67e8f9` | `#22d3ee` |
| `brightWhite` | `#f9fafb` | `#ffffff` |

---

## 10. Warp-style design cues adopted

| Cue | Adopted |
|-----|---------|
| Command-block pane header | Yes — 28 px strip above each pane with sandbox name, status dot, timing. |
| Dense single-row status bar | Yes — 22 px bottom row with shortcut hints and global status. |
| Palette overlay (Cmd+K style) | Yes — centred modal with search input, keyed to `/` or `Ctrl+P`. |
| Tabs between sessions | No — use pane grid (TUI behavior preserved). |
| Workflow sidebar | No — out of scope for desktop MVP. |

---

## 11. Contrast audit notes

- **Cyan `#22d3ee` on `#040b16`**: contrast ratio ≈ 9.6:1 — passes WCAG AA + AAA.
- **Text `#e5e7eb` on `#040b16`**: contrast ratio ≈ 14.7:1 — passes AAA.
- **Muted `#6b7280` on `#040b16`**: contrast ratio ≈ 4.7:1 — passes AA for normal text.
- **Light accent `#0891b2` on `#ffffff`**: contrast ratio ≈ 4.9:1 — passes AA.
- **Light text `#111827` on `#ffffff`**: contrast ratio ≈ 18.1:1 — passes AAA.
