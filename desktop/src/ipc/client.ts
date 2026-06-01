import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  ApiResult,
  AppBootstrap,
  EditorEntry,
  EventEnvelope,
  InputSubmitResult,
  LayoutSnapshot,
  PaneEvent,
  ProjectEntry,
  PopupEvent,
  SessionEntry,
  StatusEvent,
  TerminalEvent,
  TerminalWriteFrame,
  ThemeSnapshot,
  UiEvent,
  UploadEvent,
  WorkspaceContext
} from "../types/ipc";

function isTauriRuntime(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

function requireTauriRuntime(operation: string): void {
  if (!isTauriRuntime()) {
    throw new Error(`${operation} requires the Tauri desktop runtime`);
  }
}

function unwrap<T>(result: ApiResult<T>): T {
  if (result.ok) {
    return result.data;
  }
  const detail = result.error.detail ? ` (${result.error.detail})` : "";
  throw new Error(`[${result.error.code}] ${result.error.message}${detail}`);
}

export async function appBootstrap(): Promise<AppBootstrap> {
  requireTauriRuntime("app bootstrap");
  const result = await invoke<ApiResult<AppBootstrap>>("app_bootstrap");
  return unwrap(result);
}

export async function projectsList(): Promise<ProjectEntry[]> {
  requireTauriRuntime("projects list");
  const result = await invoke<ApiResult<ProjectEntry[]>>("projects_list");
  return unwrap(result);
}

export async function projectAddRecent(path: string): Promise<ProjectEntry> {
  requireTauriRuntime("project add recent");
  const result = await invoke<ApiResult<ProjectEntry>>("project_add_recent", {
    path
  });
  return unwrap(result);
}

export async function sessionsList(projectPath: string): Promise<SessionEntry[]> {
  requireTauriRuntime("sessions list");
  const result = await invoke<ApiResult<SessionEntry[]>>("sessions_list", {
    projectPath
  });
  return unwrap(result);
}

export async function workspaceInit(projectPath: string, sessionId?: string): Promise<WorkspaceContext> {
  requireTauriRuntime("workspace init");
  const result = await invoke<ApiResult<WorkspaceContext>>("workspace_init", {
    projectPath,
    sessionId
  });
  return unwrap(result);
}

export async function paneFocus(paneId: number): Promise<void> {
  requireTauriRuntime("pane focus");
  const result = await invoke<ApiResult<{ pane_id: number }>>("pane_focus", {
    paneId
  });
  unwrap(result);
}

export async function paneZoomToggle(paneId?: number): Promise<LayoutSnapshot> {
  requireTauriRuntime("pane zoom");
  const result = await invoke<ApiResult<LayoutSnapshot>>("pane_zoom_toggle", {
    paneId
  });
  return unwrap(result);
}

export async function paneClose(paneId: number): Promise<void> {
  requireTauriRuntime("pane close");
  const result = await invoke<ApiResult<{ handled: boolean }>>("pane_close", {
    paneId
  });
  unwrap(result);
}

export async function paneOpen(paneId: number): Promise<void> {
  requireTauriRuntime("pane open");
  const result = await invoke<ApiResult<{ handled: boolean }>>("pane_open", {
    paneId
  });
  unwrap(result);
}

export async function paneKill(paneId: number): Promise<void> {
  requireTauriRuntime("pane kill");
  const result = await invoke<ApiResult<{ handled: boolean }>>("pane_kill", {
    paneId
  });
  unwrap(result);
}

export async function inputSubmit(paneId: number, text: string): Promise<InputSubmitResult> {
  requireTauriRuntime("input submit");
  const result = await invoke<ApiResult<InputSubmitResult>>("input_submit", {
    paneId,
    text
  });
  return unwrap(result);
}

export async function commandAutocomplete(partial: string): Promise<string[]> {
  requireTauriRuntime("command autocomplete");
  const result = await invoke<ApiResult<string[]>>("command_autocomplete", { partial });
  return unwrap(result);
}

export async function terminalWrite(frame: TerminalWriteFrame): Promise<number> {
  requireTauriRuntime("terminal write");

  const result = await invoke<ApiResult<{ accepted: true; seq: number }>>("terminal_write", {
    frame
  });
  return unwrap(result).seq;
}

export async function terminalResize(paneId: number, cols: number, rows: number): Promise<void> {
  requireTauriRuntime("terminal resize");

  const result = await invoke<ApiResult<{ pane_id: number; cols: number; rows: number }>>(
    "terminal_resize",
    {
      paneId,
      cols,
      rows
    }
  );
  unwrap(result);
}

interface EventHandlers {
  onPane: (event: PaneEvent) => void;
  onTerminal: (event: TerminalEvent) => void;
  onStatus: (event: StatusEvent) => void;
  onUpload: (event: UploadEvent) => void;
  onPopup: (event: PopupEvent) => void;
  onUi: (event: UiEvent) => void;
}

export async function subscribeDesktopEvents(handlers: EventHandlers): Promise<UnlistenFn> {
  requireTauriRuntime("desktop event subscription");

  const unlistenPane = await listen<EventEnvelope<PaneEvent>>("app://pane", (event) => {
    handlers.onPane(event.payload.payload);
  });
  const unlistenTerminal = await listen<EventEnvelope<TerminalEvent>>("app://terminal", (event) => {
    handlers.onTerminal(event.payload.payload);
  });
  const unlistenStatus = await listen<EventEnvelope<StatusEvent>>("app://status", (event) => {
    handlers.onStatus(event.payload.payload);
  });
  const unlistenUpload = await listen<EventEnvelope<UploadEvent>>("app://upload", (event) => {
    handlers.onUpload(event.payload.payload);
  });
  const unlistenPopup = await listen<EventEnvelope<PopupEvent>>("app://popup", (event) => {
    handlers.onPopup(event.payload.payload);
  });
  const unlistenUi = await listen<EventEnvelope<UiEvent>>("app://ui", (event) => {
    handlers.onUi(event.payload.payload);
  });

  return () => {
    unlistenPane();
    unlistenTerminal();
    unlistenStatus();
    unlistenUpload();
    unlistenPopup();
    unlistenUi();
  };
}

export async function uploadFile(paneId: number, path: string): Promise<string> {
  requireTauriRuntime("file upload");
  const result = await invoke<ApiResult<{ upload_id: string }>>("upload_file", {
    paneId,
    path
  });
  return unwrap(result).upload_id;
}

export async function uploadPasteImage(paneId: number): Promise<string> {
  requireTauriRuntime("clipboard image upload");
  const result = await invoke<ApiResult<{ upload_id: string }>>("upload_paste_image", {
    paneId
  });
  return unwrap(result).upload_id;
}

export async function popupDismiss(popupId: string): Promise<void> {
  requireTauriRuntime("popup dismiss");
  const result = await invoke<ApiResult<{ handled: boolean }>>("popup_dismiss", {
    popupId
  });
  unwrap(result);
}

export async function popupAction(popupId: string, actionId: string): Promise<void> {
  requireTauriRuntime("popup action");
  const result = await invoke<ApiResult<{ handled: boolean }>>("popup_action", {
    popupId,
    actionId
  });
  unwrap(result);
}

export async function themeSet(name: string): Promise<ThemeSnapshot> {
  requireTauriRuntime("theme set");
  const result = await invoke<ApiResult<ThemeSnapshot>>("theme_set", { name });
  return unwrap(result);
}

export async function logsPath(): Promise<string> {
  requireTauriRuntime("logs path");
  const result = await invoke<ApiResult<{ path: string }>>("logs_path");
  return unwrap(result).path;
}

export async function editorList(): Promise<EditorEntry[]> {
  requireTauriRuntime("editor list");
  const result = await invoke<ApiResult<EditorEntry[]>>("editor_list");
  return unwrap(result);
}
