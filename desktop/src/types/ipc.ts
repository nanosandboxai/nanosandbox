export type PaneMode = "loading" | "terminal" | "headless";
export type PaneStatus = "loading" | "connected" | "disconnected" | "error";

export interface PaneSummary {
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

export interface LayoutSnapshot {
  focused_pane: number;
  hidden_panels: number[];
  zoomed_pane?: number;
  rows: number;
  cols: number;
}

export interface ThemeSnapshot {
  name: string;
  mode: "dark" | "light";
  tokens: Record<string, string>;
  xterm: Record<string, string>;
}

export interface AppBootstrap {
  version: string;
  theme: ThemeSnapshot;
  layout: LayoutSnapshot;
  panes: PaneSummary[];
  command_history_size: number;
  workspace: WorkspaceContext;
}

export interface WorkspaceContext {
  project_path?: string;
  session_id?: string;
}

export interface ProjectEntry {
  id: string;
  name: string;
  path: string;
  last_opened: string;
}

export interface SessionEntry {
  id: string;
  updated: string;
  panels: number;
  summary: string;
}

export interface ApiError {
  code: string;
  message: string;
  detail?: string;
}

export interface ApiSuccess<T> {
  ok: true;
  data: T;
}

export interface EditorEntry {
  name: string;
  binary: string;
  is_tui: boolean;
  available: boolean;
}

export interface ApiFailure {
  ok: false;
  error: ApiError;
}

export type ApiResult<T> = ApiSuccess<T> | ApiFailure;

export interface InputSubmitResult {
  kind: "message" | "command" | "command_error" | "empty";
  command_name?: string;
  message?: string;
}

export interface EventEnvelope<T> {
  version: 1;
  ts_ms: number;
  event_id: string;
  payload: T;
}

export type PaneEvent =
  | { type: "pane_snapshot"; panes: PaneSummary[]; layout: LayoutSnapshot }
  | { type: "pane_added"; pane: PaneSummary; layout: LayoutSnapshot }
  | { type: "pane_updated"; pane: PaneSummary }
  | { type: "pane_removed"; pane_id: number; layout: LayoutSnapshot }
  | { type: "focus_changed"; pane_id: number }
  | { type: "zoom_changed"; zoomed_pane?: number };

export interface TerminalWriteFrame {
  pane_id: number;
  seq: number;
  encoding: "base64";
  data: string;
}

export type TerminalEvent =
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

export interface StatusEvent {
  type: "status_update";
  running_count: number;
  loading_count: number;
  disconnected_count: number;
  focused_label: string;
}

export type UploadEvent =
  | {
      type: "upload_started";
      upload_id: string;
      pane_id: number;
      filename: string;
    }
  | {
      type: "upload_complete";
      upload_id: string;
      pane_id: number;
      filename: string;
      remote_path: string;
      size: number;
    }
  | {
      type: "upload_failed";
      upload_id: string;
      pane_id: number;
      error: string;
    };

export type UiEvent =
  | {
      type: "panel_toggle";
      target: string;
      open: boolean;
    }
  | {
      type: "clear_history";
    };

export interface PopupAction {
  id: string;
  label: string;
  primary?: boolean;
}

export type PopupEvent = {
  type: "popup_enqueue";
  popup_id: string;
  kind: "info" | "success" | "warning" | "error";
  title: string;
  body: string;
  actions: PopupAction[];
  ttl_ms?: number;
};
