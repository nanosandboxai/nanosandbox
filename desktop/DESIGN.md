# nanosb Desktop Terminal — Design

Epic 2 design package for the Tauri + Lit + xterm.js desktop terminal app.
This document is the index; each linked doc is a standalone reference for Epic 3 implementation.

## Tech stack (locked)

| Layer | Choice | Rationale |
|-------|--------|-----------|
| App shell | Tauri 2 | Native webview, small binary, plugin ecosystem |
| Frontend framework | Lit (Web Components) | Reactive primitives; Shadow/Light DOM; no heavy framework |
| Build tool | Vite + vanilla TS | Zero framework overhead; fastest HMR |
| Terminal emulator | xterm.js + WebGL addon | GPU-accelerated; large ecosystem; handles raw bytes |
| OAuth browser | Tauri secondary webview | In-app, no OS browser spawn |
| Backend logic | Rust cores from Epic 1 | `command-core`, `pane-core`, `upload-core`, `terminal-core` |
| No daemon | In-process only | IPC over Tauri commands; no socket layer |

## Design documents

| Doc | Scope |
|-----|-------|
| [`design/theme.md`](design/theme.md) | **D1** — Design tokens, semantic roles, light/dark palettes, xterm.js mapping |
| [`design/components.md`](design/components.md) | **D2** — Lit component tree, props/events, Shadow vs Light DOM |
| [`design/ipc.md`](design/ipc.md) | **D3** — Tauri commands, events, payload schemas, binary frame format |
| [`design/keymap.md`](design/keymap.md) | **D4** — Key map with TUI parity table, platform variants |
| [`design/interactions.md`](design/interactions.md) | **D5** — Grid, palette, status bar, popups, uploads, auth-URL behavior |
| [`design/local-pty.md`](design/local-pty.md) | Local PTY session kind (Phase-1.5 core follow-up) |
| [`design/adr/`](design/adr/) | **D7** — Architectural decision records for every locked choice |

## Visual mockup

[`design/mockup/`](design/mockup/) contains a self-contained static HTML/CSS prototype.
Open `design/mockup/index.html` in any browser — no build step, no server needed.

Covers: empty state, 2×2 grid, slash-command palette, system popups (info/success/warning/error),
and in-app auth browser.

## Visual baseline

- **Color family**: dd-code deep navy + cyan/emerald accents (adapted for terminal-first density).
- **Typography**: JetBrains Mono for terminal/monospace; Inter for chrome/UI.
- **Layout cues**: Warp-style 28 px command-block pane headers; dense 22 px status bar; centred palette overlay.
- **Behavior**: all panel grid, slash-command, history, popup, upload, and auth-URL behavior mirrors the existing nanosb TUI exactly (`cli/src/tui/`).

## Relationship to epics

```
Epic 1 — Core extraction (epic/core-extraction)
  └── command-core, pane-core, upload-core, terminal-core
       ↓ consumed by
Epic 2 — Terminal design (epic/terminal-design)   ← THIS BRANCH
  └── Design package: theme, components, IPC, keymap, interactions, ADRs, mockup
       ↓ input to
Epic 3 — Desktop implementation (epic/desktop-impl, TBD)
  └── cli/desktop/src-tauri/ + cli/desktop/src/ implementing this design
```

## Out of scope for Epic 2

- Any Rust or TypeScript implementation.
- Tauri project scaffolding (`src-tauri/`, `tauri.conf.json`).
- Packaging, code-signing, CI matrix.
- TUI refresh of existing `cli/src/tui/`.
- Daemon / nanosb-serve layer.
