# nanosb Desktop (Implementation)

Initial desktop implementation scaffold for Epic 3.

## Stack

- Tauri 2 (`src-tauri/`)
- Lit + TypeScript + Vite (`src/`)
- Design tokens/styles imported from `src/styles/`

## Run

```bash
npm install
npm run tauri:dev
```

Web-only preview is not supported (desktop backend required).

```bash
npm run tauri:dev
```

## Current scope

- App shell with pane grid, input bar, and status bar.
- Tauri commands: `app_bootstrap`, `pane_focus`, `input_submit`, `theme_get`, `theme_set`.
- `command-core` parser is already wired into `input_submit`.

Next implementation steps should wire terminal streams, upload flows, popups, and auth browser events from D3.
