# Desktop UI Components (D2)

Component architecture for the Tauri + Lit desktop client.

Goal: keep exact TUI behavior parity while mapping UI into clear, testable Web Components.

## 1) Component tree

```text
<nsb-app-shell>
  <nsb-pane-grid>
    <nsb-pane-cell>*
      <nsb-pane-header>
      <nsb-terminal-pane>
      <nsb-pane-overlay> (optional loading/error overlay)
  <nsb-input-bar>
  <nsb-status-bar>
  <nsb-command-palette> (portal/overlay)
  <nsb-popup-host> (portal/overlay)
  <nsb-auth-browser-shell> (separate route/window state)
```

Notes:
- `nsb-app-shell` is the only component with app-level orchestration.
- Grid/pane state must derive from `pane-core` models (no parallel UI-only layout model).
- Terminal bytes/events must flow through `terminal-core` adapters.

## 2) Component boundaries

### `nsb-app-shell`

Responsibilities:
- Own global state: panes, focus, zoom mode, palette visibility, popup queue, auth-browser visibility.
- Subscribe to Tauri event stream (`app://terminal`, `app://pane`, `app://upload`, `app://auth`).
- Dispatch user intent to Tauri commands.

Inputs:
- Boot payload from `app_bootstrap()`.

Outputs (custom events upward are not needed; this is root):
- None.

### `nsb-pane-grid`

Responsibilities:
- Render split layout from `PaneLayout` snapshot.
- Route focus click/keyboard events to root.
- Handle single-pane zoom visual mode.

Props:
- `layout: PaneLayout`
- `panes: PaneViewModel[]`
- `focusedPaneId: number`
- `zoomedPaneId: number | null`

Events:
- `pane-focus-request { paneId }`
- `pane-resize-request { direction, delta }`

### `nsb-pane-cell`

Responsibilities:
- Compose one pane frame: header + terminal + optional overlay.
- Keep visual focused/unfocused state in sync with root.

Props:
- `pane: PaneViewModel`
- `focused: boolean`

Events:
- `pane-focus-request { paneId }`

### `nsb-pane-header`

Responsibilities:
- Render pane metadata (agent label, sandbox id, panel index, runtime status dot, elapsed time).
- No logo icon in header chrome.

Props:
- `paneId: number`
- `agentName: string`
- `sandboxName: string`
- `status: "loading" | "connected" | "disconnected" | "error"`
- `elapsedMs: number`

Events:
- None (pure display for MVP).

### `nsb-terminal-pane`

Responsibilities:
- Own xterm.js instance lifecycle for a pane.
- Forward keyboard/input bytes to backend for active pane.
- Receive and write output frames.

Props:
- `paneId: number`
- `theme: TerminalTheme`
- `active: boolean`

Events:
- `terminal-input { paneId, bytes }`
- `terminal-title-change { paneId, title }` (optional)
- `terminal-url-detected { paneId, url }`

### `nsb-pane-overlay`

Responsibilities:
- Render loading/reconnect/error overlays over terminal viewport.
- Keep logo usage only for welcome/loading contexts.

Props:
- `type: "loading" | "error" | "reconnect"`
- `message: string`
- `detail?: string`
- `progress?: number`

Events:
- `overlay-action { paneId, action }`

### `nsb-input-bar`

Responsibilities:
- Render command prompt row and inline hints.
- Capture slash commands and free text.

Props:
- `prompt: string` (default `>>`)
- `value: string`
- `placeholder: string`
- `modeBadge?: string`

Events:
- `input-change { value }`
- `input-submit { value }`
- `palette-toggle-request`

### `nsb-status-bar`

Responsibilities:
- Render global status counts and shortcut hints.
- Display focused pane summary.

Props:
- `version: string`
- `runningCount: number`
- `loadingCount: number`
- `disconnectedCount: number`
- `focusedLabel: string`

Events:
- None for MVP.

### `nsb-command-palette`

Responsibilities:
- Render search input + grouped command results.
- Manage keyboard navigation (`Up`, `Down`, `Enter`, `Esc`).

Props:
- `open: boolean`
- `query: string`
- `items: CommandPaletteItem[]`
- `selectedIndex: number`

Events:
- `palette-query-change { query }`
- `palette-select { itemId }`
- `palette-close`

### `nsb-popup-host`

Responsibilities:
- Display one modal popup at a time (info/success/warning/error).
- Queue behavior for bursty backend notifications.

Props:
- `activePopup: PopupViewModel | null`

Events:
- `popup-dismiss { popupId }`
- `popup-action { popupId, actionId }`

### `nsb-auth-browser-shell`

Responsibilities:
- Render in-app browser chrome and container for secondary webview.
- Handle OAuth code flow status and timeout UI.

Props:
- `open: boolean`
- `url: string`
- `title?: string`
- `expiresAt?: number`

Events:
- `auth-close-request`
- `auth-nav { action: "back" | "forward" | "reload" }`

## 3) Shadow DOM vs Light DOM

Use Lit with selective shadow boundaries.

Use Shadow DOM:
- `nsb-input-bar`
- `nsb-status-bar`
- `nsb-command-palette`
- `nsb-popup-host`
- `nsb-auth-browser-shell`

Use Light DOM (`createRenderRoot() { return this; }`):
- `nsb-app-shell`
- `nsb-pane-grid`
- `nsb-pane-cell`
- `nsb-pane-header`
- `nsb-terminal-pane`
- `nsb-pane-overlay`

Rationale:
- xterm.js and split/grid sizing are easier in Light DOM.
- Overlays and controls benefit from style encapsulation.
- Global design tokens still come from `:root` CSS variables in `src/styles/tokens.css`.

## 4) State ownership and data flow

Single source of truth:
- Root app store in `nsb-app-shell`.
- No child component mutates canonical pane state.

Flow:
1. User action from component emits custom event.
2. `nsb-app-shell` translates to Tauri command.
3. Rust core updates state.
4. Tauri emits event payload.
5. Root store updates and pushes props down.

## 5) Suggested TypeScript view models

```ts
type PaneStatus = "loading" | "connected" | "disconnected" | "error";

interface PaneViewModel {
  paneId: number;
  agentName: string;
  sandboxName: string;
  status: PaneStatus;
  elapsedMs: number;
  focused: boolean;
  overlay?: {
    type: "loading" | "error" | "reconnect";
    message: string;
    detail?: string;
    progress?: number;
  };
}

interface CommandPaletteItem {
  id: string;
  command: string;
  description: string;
  group: "agents" | "options" | "commands";
}

interface PopupViewModel {
  id: string;
  kind: "info" | "success" | "warning" | "error";
  title: string;
  body: string;
  actions: Array<{ id: string; label: string; primary?: boolean }>;
}
```

## 6) Accessibility and keyboard rules

- Focus order: pane grid -> input bar -> palette -> popup -> auth browser.
- Palette and popup must trap focus while open.
- `Esc` closes palette, then popup, then auth browser (in that order).
- Status dot requires text equivalent in header (`aria-label` with status).
- Shortcut hints displayed in status/input bars must match actual bindings from D4.

## 7) Mapping to mockup artifacts

- Grid and pane chrome: `design/mockup/grid.html`
- Palette interaction model: `design/mockup/palette.html`
- Popup variants: `design/mockup/popup.html`
- In-app auth browser shell: `design/mockup/auth-browser.html`
- Welcome/loading logo usage: `design/mockup/index.html` and loading overlay states

## 8) Non-goals in D2

- No final IPC schema definitions (covered in D3).
- No keybinding matrix (covered in D4).
- No implementation code in `cli/desktop/src/` yet.
