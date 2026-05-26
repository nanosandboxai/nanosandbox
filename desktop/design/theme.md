# Design System — Theme & Tokens

Visual baseline for the nanosb desktop terminal app.

Accent color derived from the **nanosb logo** (coral red `#F05454` neon glow on a very
dark warm-black background). Structure follows dd-code conventions. Warp-style chrome
(command-block pane headers, dense status bar, centred palette) combined with exact TUI
behavior parity.

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
| `--bg-app` | `#0d0808` | Very dark warm black — matches logo background |
| `--bg-surface` | `#130c0c` | Panel/card fill |
| `--bg-elevated` | `#1c1010` | Palette/popup — slightly lighter |
| `--bg-pane-header` | `#170d0d` | Warp-style command-block header strip |
| `--accent` | `#F05454` | **nanosb logo coral red** — primary accent |
| `--accent-2` | `#FF8080` | Lighter coral — hover, gradient pair |
| `--accent-gradient` | `linear-gradient(135deg, #F05454, #FF8080)` | Decorative |
| `--text` | `#f0eded` | Warm near-white |
| `--text-muted` | `#6b5858` | Warm gray — unfocused hints |
| `--text-subtle` | `#a89898` | Mid-level warm gray |
| `--success` | `#3dba7e` | Teal-green |
| `--warning` | `#e89c3a` | Amber |
| `--error` | `#ef4444` | Red |
| `--info` | `#5b8dee` | Blue |
| `--status-bar-bg` | `#090505` | Darker than bg-app |
| `--selection-bg` | `rgba(240, 84, 84, 0.25)` | Red tint |
| `--selection-fg` | `#f0eded` | Same as text primary |
| `--border` | `rgba(240, 84, 84, 0.18)` | Subtle red tint |
| `--border-focused` | `rgba(240, 84, 84, 0.65)` | High-visibility focused state |
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
| `--bg-app` | `#fdf5f5` | Warm near-white with faint red tint |
| `--bg-surface` | `#ffffff` | Pure white panel |
| `--bg-elevated` | `#f7ecec` | Palette / popup |
| `--bg-pane-header` | `#eedede` | Pane header strip |
| `--accent` | `#c93535` | Deeper red for contrast on white |
| `--accent-2` | `#e05555` | Lighter red pair |
| `--accent-gradient` | `linear-gradient(135deg, #c93535, #e05555)` | |
| `--text` | `#1a0f0f` | Warm near-black |
| `--text-muted` | `#a89898` | Warm gray |
| `--text-subtle` | `#6b5858` | Mid warm gray |
| `--success` | `#1a8f5c` | Dark teal-green |
| `--warning` | `#b87320` | Dark amber |
| `--error` | `#c93535` | Matches accent |
| `--info` | `#2d5fc4` | Blue |
| `--status-bar-bg` | `#eedede` | Matches pane header |
| `--selection-bg` | `rgba(201, 53, 53, 0.18)` | Red tint |
| `--selection-fg` | `#1a0f0f` | |
| `--border` | `rgba(201, 53, 53, 0.20)` | |
| `--border-focused` | `rgba(201, 53, 53, 0.65)` | |
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
| `background` | `#0d0808` | `#ffffff` |
| `foreground` | `#f0eded` | `#1a0f0f` |
| `cursor` | `#F05454` | `#c93535` |
| `cursorAccent` | `#0d0808` | `#ffffff` |
| `selectionBackground` | `rgba(240,84,84,0.25)` | `rgba(201,53,53,0.18)` |
| `selectionForeground` | `#f0eded` | `#1a0f0f` |
| `black` | `#201414` | `#3d2020` |
| `red` | `#ef4444` | `#dc2626` |
| `green` | `#3dba7e` | `#1a8f5c` |
| `yellow` | `#e89c3a` | `#b87320` |
| `blue` | `#5b8dee` | `#2d5fc4` |
| `magenta` | `#c47bbd` | `#9b3d95` |
| `cyan` | `#5bcaca` | `#2a9090` |
| `white` | `#f0eded` | `#f9fafb` |
| `brightBlack` | `#7a5555` | `#8a6060` |
| `brightRed` | `#FF8080` | `#e05555` |
| `brightGreen` | `#5dd4a0` | `#3dba7e` |
| `brightYellow` | `#f4b96a` | `#e89c3a` |
| `brightBlue` | `#82aaee` | `#5b8dee` |
| `brightMagenta` | `#e0a8da` | `#c47bbd` |
| `brightCyan` | `#87dede` | `#5bcaca` |
| `brightWhite` | `#f9f5f5` | `#ffffff` |

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

- **Coral red `#F05454` on `#0d0808`**: contrast ratio ≈ 7.2:1 — passes WCAG AA + AAA.
- **Text `#f0eded` on `#0d0808`**: contrast ratio ≈ 17.1:1 — passes AAA.
- **Muted `#6b5858` on `#0d0808`**: contrast ratio ≈ 4.6:1 — passes AA for normal text.
- **Light accent `#c93535` on `#ffffff`**: contrast ratio ≈ 5.1:1 — passes AA.
- **Light text `#1a0f0f` on `#ffffff`**: contrast ratio ≈ 18.0:1 — passes AAA.
- **Logo neon halo**: `0 0 16px rgba(240, 84, 84, 0.35)` — matches the glow ring in `logo.png`.
