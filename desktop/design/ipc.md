# IPC Contract (D3)

Runtime contract between the desktop frontend (Lit/TS) and the Rust backend in Tauri.

Scope:
- `invoke` command surface (frontend -> Rust)
- event channels (Rust -> frontend)
- payload schemas for pane/terminal/upload/popup/auth
- frame format for terminal byte streams

Out of scope:
- Rust implementation details (Epic 3)
- keybinding matrix (D4)

## 1) Principles

- In-process architecture only; no daemon/socket transport.
- Backend remains source of truth for pane/session state.
- Frontend sends intents; backend emits authoritative snapshots/patches.
- Terminal traffic is byte-oriented and panel-scoped.
- Every command returns typed `Result<T, ApiError>`.

## 2) Naming and channels

Tauri invoke namespace (recommended):
- `app_*` for bootstrap/global metadata
- `pane_*` for pane lifecycle/layout/focus
- `input_*` for command-bar submit
- `palette_*` for slash-command UX
- `terminal_*` for PTY byte stream and resize
- `upload_*` for host -> sandbox transfer
- `auth_*` for in-app OAuth browser flows
- `popup_*` for popup acknowledgement/actions

Tauri event channels:
- `app://pane`
- `app://terminal`
- `app://upload`
- `app://auth`
- `app://popup`
- `app://status`

## 3) Shared envelope types

```ts
interface ApiError {
  code:
    | "BAD_REQUEST"
    | "NOT_FOUND"
    | "CONFLICT"
    | "UNSUPPORTED"
    | "INTERNAL"
    | "SSH_DISCONNECTED"
    | "UPLOAD_TOO_LARGE"
    | "PERMISSION_DENIED";
  message: string;
  detail?: string;
}

interface Ok<T> {
  ok: true;
  data: T;
}

interface Err {
  ok: false;
  error: ApiError;
}

type ApiResult<T> = Ok<T> | Err;

interface EventEnvelope<T> {
  version: 1;
  ts_ms: number;
  event_id: string;
  payload: T;
}
```

## 4) Core DTOs

```ts
type PaneMode = "loading" | "terminal" | "headless";
type PaneStatus = "loading" | "connected" | "disconnected" | "error";

interface PaneSummary {
  pane_id: number;
  agent_name: string;
  display_name?: string;
  sandbox_id_short: string;
  mode: PaneMode;
  status: PaneStatus;
  visible: boolean;
  focused: boolean;
  reconnecting: boolean;
  loading_message?: string;
  loading_error?: string;
  elapsed_ms?: number;
}

interface LayoutSnapshot {
  focused_pane: number;
  hidden_panels: number[];
  zoomed_pane?: number;
  rows: number;
  cols: number;
}

interface AppBootstrap {
  version: string;
  theme: ThemeSnapshot;
  layout: LayoutSnapshot;
  panes: PaneSummary[];
  command_history_size: number;
}

interface ThemeSnapshot {
  name: string;
  mode: "dark" | "light";
  tokens: Record<string, string>;
  xterm: Record<string, string>;
}

interface PaletteItem {
  item_id: string;
  command: string;
  description: string;
  group: "agents" | "options" | "commands";
}
```

## 5) Frontend -> backend commands (`invoke`)

### App

1) `app_bootstrap()` -> `ApiResult<AppBootstrap>`
- Load initial state needed before first render.

2) `theme_get()` -> `ApiResult<ThemeSnapshot>`
- Returns semantic tokens and xterm theme mapping from `design/theme.md`.

3) `theme_set({ name })` -> `ApiResult<{ name: string }>`
- Switch active theme.

### Pane/layout

4) `pane_list()` -> `ApiResult<{ panes: PaneSummary[]; layout: LayoutSnapshot }>`

5) `pane_add_agent(args)` -> `ApiResult<{ pane_id: number }>`

```ts
interface PaneAddAgentArgs {
  agent: "claude" | "goose" | "codex" | "cursor";
  image?: string;
  tag?: string;
  project?: string;
  branch?: string;
  name?: string;
  auto_mode?: boolean;
  prompt?: string;
  model?: string;
  use_env?: string[];
  env_file?: string;
  run_as_root?: boolean;
}
```

6) `pane_focus({ pane_id })` -> `ApiResult<{ pane_id: number }>`

7) `pane_zoom_toggle()` -> `ApiResult<{ zoomed_pane?: number }>`

8) `pane_close({ target? })` -> `ApiResult<{ closed_pane: number }>`

9) `pane_open({ target? })` -> `ApiResult<{ opened_pane: number }>`

10) `pane_reconnect({ pane_id? })` -> `ApiResult<{ pane_id: number }>`

11) `pane_kill({ target? })` -> `ApiResult<{ removed_pane: number }>`

### Input + slash-command

12) `input_submit({ pane_id, text })` -> `ApiResult<InputSubmitResult>`

```ts
interface InputSubmitResult {
  kind: "message" | "command" | "command_error" | "empty";
  command_name?: string;
  message?: string;
}
```

13) `palette_suggest({ query })` -> `ApiResult<{ items: PaletteItem[] }>`

14) `palette_execute({ item_id })` -> `ApiResult<{ executed: true }>`

### Terminal transport

15) `terminal_write(frame)` -> `ApiResult<{ accepted: true; seq: number }>`

16) `terminal_resize({ pane_id, cols, rows })` -> `ApiResult<{ pane_id: number; cols: number; rows: number }>`

17) `terminal_scroll({ pane_id, action, lines? })` -> `ApiResult<{ pane_id: number; offset: number }>`

```ts
interface TerminalWriteFrame {
  pane_id: number;
  seq: number;
  encoding: "base64";
  data: string;
}
```

### Upload

18) `upload_file({ pane_id, path })` -> `ApiResult<{ upload_id: string }>`

19) `upload_paste_image({ pane_id })` -> `ApiResult<{ upload_id: string }>`

### Auth browser

20) `auth_open({ pane_id, url })` -> `ApiResult<{ auth_id: string }>`

21) `auth_close({ auth_id })` -> `ApiResult<{ closed: true }>`

22) `auth_nav({ auth_id, action })` -> `ApiResult<{ action: "back" | "forward" | "reload" }>`

### Popup

23) `popup_dismiss({ popup_id })` -> `ApiResult<{ dismissed: true }>`

24) `popup_action({ popup_id, action_id })` -> `ApiResult<{ handled: true }>`

## 6) Backend -> frontend events

Each event payload is wrapped in `EventEnvelope<T>`.

### `app://pane`

```ts
type PaneEvent =
  | { type: "pane_snapshot"; panes: PaneSummary[]; layout: LayoutSnapshot }
  | { type: "pane_added"; pane: PaneSummary; layout: LayoutSnapshot }
  | { type: "pane_updated"; pane: PaneSummary }
  | { type: "pane_removed"; pane_id: number; layout: LayoutSnapshot }
  | { type: "focus_changed"; pane_id: number }
  | { type: "zoom_changed"; zoomed_pane?: number };
```

### `app://terminal`

```ts
type TerminalEvent =
  | {
      type: "terminal_data";
      pane_id: number;
      seq: number;
      encoding: "base64";
      data: string;
    }
  | {
      type: "terminal_disconnected";
      pane_id: number;
      error?: string;
    }
  | {
      type: "terminal_connected";
      pane_id: number;
      cols: number;
      rows: number;
    };
```

### `app://upload`

```ts
type UploadEvent =
  | { type: "upload_started"; upload_id: string; pane_id: number; filename: string }
  | {
      type: "upload_progress";
      upload_id: string;
      pane_id: number;
      sent_bytes: number;
      total_bytes: number;
    }
  | {
      type: "upload_complete";
      upload_id: string;
      pane_id: number;
      filename: string;
      remote_path: string;
      size: number;
    }
  | { type: "upload_failed"; upload_id: string; pane_id: number; error: string };
```

### `app://auth`

```ts
type AuthEvent =
  | {
      type: "auth_url_detected";
      pane_id: number;
      url: string;
      dedup_key: string;
      auto_open: false;
    }
  | { type: "auth_opened"; auth_id: string; pane_id: number; url: string }
  | { type: "auth_completed"; auth_id: string; pane_id: number }
  | { type: "auth_failed"; auth_id: string; pane_id: number; error: string }
  | { type: "auth_closed"; auth_id: string; pane_id: number };
```

### `app://popup`

```ts
type PopupKind = "info" | "success" | "warning" | "error";

interface PopupAction {
  id: string;
  label: string;
  primary?: boolean;
}

type PopupEvent = {
  type: "popup_enqueue";
  popup_id: string;
  kind: PopupKind;
  title: string;
  body: string;
  actions: PopupAction[];
  ttl_ms?: number;
};
```

### `app://status`

```ts
type StatusEvent = {
  type: "status_update";
  running_count: number;
  loading_count: number;
  disconnected_count: number;
  focused_label: string;
};
```

## 7) Terminal frame and ordering rules

- Transport payload for terminal bytes uses base64 strings.
- `seq` is per-pane, monotonic, starts at 1 on each connection.
- Frontend drops stale/out-of-order frames (`seq <= last_seq`).
- Terminal data events are append-only; backend does not emit screen diffs.
- Resize is last-write-wins; frontend should debounce resize calls (50-80ms).

## 8) Error mapping guidelines

Recommended mappings:
- Upload > `MAX_UPLOAD_SIZE` -> `UPLOAD_TOO_LARGE`
- Missing pane id -> `NOT_FOUND`
- Pane exists but disconnected terminal write -> `SSH_DISCONNECTED`
- Invalid slash command -> `BAD_REQUEST` with parse help in `detail`
- Unsupported action for current mode -> `UNSUPPORTED`

## 9) Compatibility/versioning

- All event envelopes include `version: 1`.
- Breaking schema changes require version bump and explicit migration note.
- Additive fields are allowed in minor updates.

## 10) Minimal startup sequence

1. Frontend calls `app_bootstrap()`.
2. Frontend calls `theme_get()` and applies xterm theme.
3. Frontend subscribes to all `app://*` channels.
4. Frontend renders grid from bootstrap snapshot.
5. User input flows through `input_submit()` and/or `terminal_write()`.
