import { LitElement, html } from "lit";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalSize } from "@tauri-apps/api/dpi";
import {
  appBootstrap,
  commandAutocomplete,
  editorList,
  inputSubmit,
  logsPath,
  paneClose,
  paneFocus,
  paneKill,
  paneOpen,
  paneZoomToggle,
  projectAddRecent,
  projectsList,
  popupAction,
  popupDismiss,
  sessionsList,
  subscribeDesktopEvents,
  themeSet,
  workspaceInit,
  uploadPasteImage
} from "../ipc/client";
import type {
  AppBootstrap,
  ProjectEntry,
  PaneEvent,
  EditorEntry,
  PopupAction,
  PopupEvent,
  SessionEntry,
  StatusEvent,
  TerminalEvent,
  UiEvent,
  UploadEvent,
  WorkspaceContext
} from "../types/ipc";
import type { PaletteItem } from "./nsb-command-palette";
import "./nsb-pane-grid";
import "./nsb-command-palette";
import "./nsb-popup-host";
import "./nsb-sandbox-panel";

interface TerminalPaneElement extends HTMLElement {
  appendBase64Data: (seq: number, base64Data: string) => void;
  focusTerminal?: () => void;
  copySelectionOrVisibleBuffer?: () => string;
}

export class NsbAppShell extends LitElement {
  static properties = {
    bootstrap: { state: true },
    inputValue: { state: true },
    lastSubmitMessage: { state: true },
    loadError: { state: true },
    statusLine: { state: true },
    activePopup: { state: true },
    zoomedPaneId: { state: true },
    settingsOpen: { state: true },
    sandboxPanelOpen: { state: true },
    selectedSandboxPaneId: { state: true },
    paletteOpen: { state: true },
    paletteQuery: { state: true },
    paletteSelectedIndex: { state: true },
    commandSuggestions: { state: true },
    commandSuggestionIndex: { state: true },
    copyToastMessage: { state: true },
    logsPathValue: { state: true },
    availableEditors: { state: true },
    workspaceContext: { state: true },
    startupProjects: { state: true },
    startupSessions: { state: true },
    startupProjectPath: { state: true },
    startupSessionId: { state: true },
    startupBusy: { state: true },
    switcherOpen: { state: true },
    switcherProjectPath: { state: true },
    switcherSessionId: { state: true },
    switcherSessions: { state: true }
  };

  protected createRenderRoot(): HTMLElement {
    return this;
  }

  private bootstrap: AppBootstrap | null = null;

  private inputValue = "";

  private lastSubmitMessage = "";

  private loadError = "";

  private statusLine = "";

  private unlistenDesktopEvents: (() => void) | null = null;

  private paletteOpen = false;

  private paletteQuery = "";

  private paletteSelectedIndex = 0;

  private paletteItems: PaletteItem[] = [
    { item_id: "add-claude", command: "/add claude", description: "Add Claude agent panel", category: "Panels" },
    { item_id: "add-goose", command: "/add goose", description: "Add Goose agent panel", category: "Panels" },
    { item_id: "upload", command: "/upload <path>", description: "Upload host file", category: "IO" },
    { item_id: "help", command: "/help", description: "Show command help", category: "General" }
  ];

  private activePopup: PopupEvent | null = null;

  private zoomedPaneId: number | null = null;

  private settingsOpen = false;

  private sandboxPanelOpen = false;

  private selectedSandboxPaneId: number | null = null;

  private commandSuggestions: string[] = [];

  private commandSuggestionIndex = 0;

  private commandSuggestReqId = 0;

  private commandHistory: string[] = [];

  private commandHistoryIndex = -1;

  private commandHistoryDraft = "";

  private readonly commandHistoryStorageKey = "nanosb-desktop-command-history";

  private logsPathValue = "";

  private availableEditors: string[] = [];

  private copyToastMessage = "";

  private copyToastTimer: number | null = null;

  private workspaceContext: WorkspaceContext | null = null;

  private startupProjects: ProjectEntry[] = [];

  private startupSessions: SessionEntry[] = [];

  private startupProjectPath = "";

  private startupSessionId = "__new__";

  private startupBusy = false;

  private launchAgentName: string | null = null;

  private switcherOpen = false;

  private switcherProjectPath = "";

  private switcherSessionId = "__new__";

  private switcherSessions: SessionEntry[] = [];

  private readonly themeOptions = [
    "nanosandbox",
    "nanosandbox-light",
    "dracula",
    "catppuccin",
    "tokyo-night",
    "nord"
  ];

  private onUnhandledRejection = (event: PromiseRejectionEvent): void => {
    const reason = event.reason instanceof Error ? event.reason.message : String(event.reason ?? "Unknown error");
    this.reportUiError(`Unhandled promise rejection: ${reason}`, "Unhandled Error");
  };

  private onWindowError = (event: ErrorEvent): void => {
    const reason = event.error instanceof Error ? event.error.message : event.message;
    this.reportUiError(`Window error: ${reason || "Unknown error"}`, "Unhandled Error");
  };

  private showLocalPopup(kind: PopupEvent["kind"], title: string, body: string, actions?: PopupAction[]): void {
    this.activePopup = {
      type: "popup_enqueue",
      popup_id: `local-${Date.now()}`,
      kind,
      title,
      body,
      actions:
        actions ??
        [
          {
            id: "dismiss",
            label: "Dismiss",
            primary: true
          }
        ]
    };
  }

  private reportUiError(error: unknown, title = "Action Failed"): void {
    const message = error instanceof Error ? error.message : String(error);
    this.lastSubmitMessage = message;
    this.showLocalPopup("error", title, message);
  }

  private loadCommandHistory(): void {
    try {
      const raw = window.localStorage.getItem(this.commandHistoryStorageKey);
      if (!raw) {
        this.commandHistory = [];
        return;
      }
      const parsed = JSON.parse(raw);
      if (!Array.isArray(parsed)) {
        this.commandHistory = [];
        return;
      }
      this.commandHistory = parsed
        .filter((value): value is string => typeof value === "string")
        .slice(0, 200);
    } catch {
      this.commandHistory = [];
    }
  }

  private persistCommandHistory(): void {
    window.localStorage.setItem(this.commandHistoryStorageKey, JSON.stringify(this.commandHistory.slice(0, 200)));
  }

  private clearCommandHistory(): void {
    this.commandHistory = [];
    this.commandHistoryIndex = -1;
    this.commandHistoryDraft = "";
    this.persistCommandHistory();
  }

  private pushCommandHistory(entry: string): void {
    const trimmed = entry.trim();
    if (!trimmed.startsWith("/")) {
      return;
    }
    if (this.commandHistory[0] === trimmed) {
      this.commandHistoryIndex = -1;
      this.commandHistoryDraft = "";
      return;
    }
    this.commandHistory = [trimmed, ...this.commandHistory.filter((value) => value !== trimmed)].slice(0, 200);
    this.commandHistoryIndex = -1;
    this.commandHistoryDraft = "";
    this.persistCommandHistory();
  }

  async connectedCallback(): Promise<void> {
    super.connectedCallback();
    try {
      this.bootstrap = await appBootstrap();
      this.loadCommandHistory();
      this.selectedSandboxPaneId = this.bootstrap.layout.focused_pane;
      this.workspaceContext = this.bootstrap.workspace?.project_path ? this.bootstrap.workspace : null;
      this.switcherProjectPath = this.bootstrap.workspace?.project_path ?? "";
      this.switcherSessionId = this.bootstrap.workspace?.session_id ?? "__new__";
      try {
        const commands = await commandAutocomplete("");
        this.paletteItems = commands.map((command) => ({
          item_id: command,
          command,
          description: this.describeCommand(command),
          category: this.commandCategory(command)
        }));
      } catch {
        // Keep default palette entries if autocomplete bootstrap fails.
      }
      document.documentElement.dataset.theme = this.bootstrap.theme.mode;
      this.logsPathValue = await logsPath();
      await this.refreshEditors();
      await this.loadStartupProjects();
      await this.loadSwitcherSessions();
      if (!this.isWorkspaceReady()) {
        await this.resizeWindow(920, 640);
      }

      this.unlistenDesktopEvents = await subscribeDesktopEvents({
        onPane: (event) => this.handlePaneEvent(event),
        onTerminal: (event) => this.handleTerminalEvent(event),
        onStatus: (event) => this.handleStatusEvent(event),
        onUpload: (event) => this.handleUploadEvent(event),
        onPopup: (event) => this.handlePopupEvent(event),
        onUi: (event) => this.handleUiEvent(event)
      });

      window.addEventListener("keydown", this.onGlobalKeyDown);
      window.addEventListener("unhandledrejection", this.onUnhandledRejection);
      window.addEventListener("error", this.onWindowError);
    } catch (error) {
      this.loadError = error instanceof Error ? error.message : "Failed to bootstrap app";
    }
  }

  disconnectedCallback(): void {
    super.disconnectedCallback();
    if (this.unlistenDesktopEvents) {
      this.unlistenDesktopEvents();
      this.unlistenDesktopEvents = null;
    }
    window.removeEventListener("keydown", this.onGlobalKeyDown);
    window.removeEventListener("unhandledrejection", this.onUnhandledRejection);
    window.removeEventListener("error", this.onWindowError);
    if (this.copyToastTimer) {
      window.clearTimeout(this.copyToastTimer);
      this.copyToastTimer = null;
    }
  }

  private onGlobalKeyDown = (event: KeyboardEvent): void => {
    if (event.key === "Escape" && this.commandSuggestions.length) {
      this.commandSuggestions = [];
      this.commandSuggestionIndex = 0;
      return;
    }

    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "p") {
      event.preventDefault();
      this.paletteOpen = true;
      return;
    }

    if (event.key === "Escape") {
      if (this.paletteOpen) {
        this.paletteOpen = false;
        return;
      }
      if (this.activePopup) {
        this.activePopup = null;
        return;
      }
      if (this.sandboxPanelOpen) {
        this.sandboxPanelOpen = false;
        return;
      }
      if (this.switcherOpen) {
        this.switcherOpen = false;
        return;
      }
      if (this.settingsOpen) {
        this.settingsOpen = false;
        return;
      }
    }
  };

  private handlePaneEvent(event: PaneEvent): void {
    if (!this.bootstrap) {
      return;
    }

    if (event.type === "pane_snapshot") {
      this.zoomedPaneId = event.layout.zoomed_pane ?? null;
      const fallbackFocusedPane =
        event.panes.find((pane) => pane.pane_id === event.layout.focused_pane && pane.visible)?.pane_id ??
        event.panes.find((pane) => pane.visible)?.pane_id ??
        null;
      this.bootstrap = {
        ...this.bootstrap,
        panes: event.panes,
        layout: {
          ...event.layout,
          focused_pane: fallbackFocusedPane ?? event.layout.focused_pane
        }
      };
      this.selectedSandboxPaneId = fallbackFocusedPane ?? event.layout.focused_pane;
      if (fallbackFocusedPane != null) {
        this.focusPaneTerminal(fallbackFocusedPane);
      }
      if (fallbackFocusedPane != null && fallbackFocusedPane !== event.layout.focused_pane) {
        void paneFocus(fallbackFocusedPane).catch((error) => {
          this.reportUiError(error, "Focus Reconcile Failed");
        });
      }
      return;
    }

    if (event.type === "focus_changed") {
      const panes = this.bootstrap.panes.map((pane) => ({
        ...pane,
        focused: pane.pane_id === event.pane_id
      }));
      this.bootstrap = {
        ...this.bootstrap,
        panes,
        layout: {
          ...this.bootstrap.layout,
          focused_pane: event.pane_id
        }
      };
      this.selectedSandboxPaneId = event.pane_id;
      this.focusPaneTerminal(event.pane_id);
      return;
    }

    if (event.type === "zoom_changed") {
      this.zoomedPaneId = event.zoomed_pane ?? null;
    }
  }

  private handleTerminalEvent(event: TerminalEvent): void {
    if (event.type !== "terminal_data") {
      return;
    }

    const paneEl = this.querySelector<TerminalPaneElement>(`nsb-terminal-pane[pane-id="${event.pane_id}"]`);
    if (!paneEl || typeof paneEl.appendBase64Data !== "function") {
      return;
    }

    paneEl.appendBase64Data(event.seq, event.data);
  }

  private handleStatusEvent(event: StatusEvent): void {
    this.statusLine = `${event.running_count} running · ${event.loading_count} loading · ${event.disconnected_count} disconnected · ${event.focused_label}`;
  }

  private handleUploadEvent(event: UploadEvent): void {
    if (event.type === "upload_complete") {
      this.lastSubmitMessage = `Uploaded ${event.filename}`;
    }
    if (event.type === "upload_failed") {
      this.lastSubmitMessage = event.error;
    }
  }

  private onPaneCopy(event: CustomEvent<{ paneId: number }>): void {
    const pane = this.bootstrap?.panes.find((item) => item.pane_id === event.detail.paneId);
    const label = pane ? `${pane.agent_name} · ${pane.sandbox_id_short}` : `panel ${event.detail.paneId}`;
    this.copyToastMessage = `Copied from ${label}`;
    if (this.copyToastTimer) {
      window.clearTimeout(this.copyToastTimer);
    }
    this.copyToastTimer = window.setTimeout(() => {
      this.copyToastMessage = "";
      this.copyToastTimer = null;
    }, 1500);
  }

  private focusPaneTerminal(paneId: number): void {
    requestAnimationFrame(() => {
      const paneEl = this.querySelector<TerminalPaneElement>(`nsb-terminal-pane[pane-id="${paneId}"]`);
      paneEl?.focusTerminal?.();
    });
  }

  private handlePopupEvent(event: PopupEvent): void {
    this.activePopup = event;
    if (event.ttl_ms && event.ttl_ms > 0) {
      window.setTimeout(() => {
        if (this.activePopup?.popup_id === event.popup_id) {
          this.activePopup = null;
        }
      }, event.ttl_ms);
    }
  }

  private handleUiEvent(event: UiEvent): void {
    if (event.type === "panel_toggle" && event.target === "sandboxes") {
      this.settingsOpen = false;
      this.sandboxPanelOpen = event.open ? !this.sandboxPanelOpen : false;
      if (this.sandboxPanelOpen && this.bootstrap) {
        this.selectedSandboxPaneId = this.bootstrap.layout.focused_pane;
      }
      return;
    }
    if (event.type === "clear_history") {
      this.clearCommandHistory();
    }
  }

  private describeCommand(command: string): string {
    const [head] = command.split(" ");
    switch (head) {
      case "/add":
        return "Add new agent panel";
      case "/focus":
        return "Focus a panel by index";
      case "/close":
        return "Hide panel";
      case "/open":
        return "Show hidden panel";
      case "/kill":
        return "Kill sandbox panel";
      case "/zoom":
        return "Toggle panel maximize";
      case "/upload":
        return "Upload host file";
      case "/paste-image":
        return "Paste clipboard image";
      case "/reconnect":
        return "Reconnect SSH";
      case "/theme":
        return "Switch theme";
      case "/env":
        return "List or set env vars";
      case "/sandboxes":
        return "Open sandbox panel";
      case "/copy":
        return "Copy focused output";
      case "/branches":
        return "List branches";
      case "/gitsync":
        return "Git sync controls";
      case "/edit":
        return "Open clone directory";
      case "/mcp":
        return "Manage MCP servers";
      case "/skills":
        return "Manage active skills";
      case "/agent":
        return "Agent settings";
      case "/clearhistory":
        return "Clear command history";
      case "/help":
        return "Show help";
      case "/destroy":
        return "Destroy all panels";
      case "/quit":
      case "/q":
        return "Exit desktop app";
      default:
        return "Run command";
    }
  }

  private commandCategory(command: string): string {
    const [head] = command.split(" ");
    switch (head) {
      case "/add":
      case "/focus":
      case "/close":
      case "/open":
      case "/kill":
      case "/zoom":
      case "/destroy":
        return "Panels";
      case "/upload":
      case "/paste-image":
      case "/copy":
        return "IO";
      case "/theme":
      case "/help":
      case "/clearhistory":
      case "/quit":
      case "/q":
      case "/sandboxes":
        return "General";
      case "/reconnect":
      case "/branches":
      case "/gitsync":
      case "/edit":
        return "Workspace";
      case "/env":
      case "/mcp":
      case "/skills":
      case "/agent":
        return "Runtime";
      default:
        return "General";
    }
  }

  private pickEditorNames(editors: EditorEntry[]): string[] {
    const preferred = editors
      .filter((editor) => editor.available && !editor.is_tui)
      .map((editor) => editor.name);
    const allAvailable = editors.filter((editor) => editor.available).map((editor) => editor.name);
    const fallback = editors.map((editor) => editor.name);
    const source = preferred.length ? preferred : allAvailable.length ? allAvailable : fallback;
    return [...new Set(source)];
  }

  private async refreshEditors(): Promise<void> {
    try {
      const editors = await editorList();
      this.availableEditors = this.pickEditorNames(editors);
    } catch {
      this.availableEditors = [];
    }
  }

  private isWorkspaceReady(): boolean {
    return Boolean(this.workspaceContext?.project_path);
  }

  private async resizeWindow(width: number, height: number): Promise<void> {
    try {
      const current = getCurrentWindow();
      await current.setSize(new LogicalSize(width, height));
      await current.center();
    } catch {
      // Ignore size failures in non-Tauri preview.
    }
  }

  private async loadStartupProjects(): Promise<void> {
    try {
      const projects = await projectsList();
      this.startupProjects = projects;
      if (!projects.length) {
        this.startupProjectPath = "";
        this.startupSessions = [];
        this.startupSessionId = "__new__";
        return;
      }

      const selected = projects.find((item) => item.path === this.startupProjectPath) ?? projects[0];
      this.startupProjectPath = selected.path;
      await this.loadStartupSessions(selected.path);
    } catch (error) {
      this.reportUiError(error, "Failed to load projects");
    }
  }

  private async loadStartupSessions(projectPath: string): Promise<void> {
    try {
      const sessions = await sessionsList(projectPath);
      this.startupSessions = sessions;
      this.startupSessionId = sessions[0]?.id ?? "__new__";
    } catch (error) {
      this.startupSessions = [];
      this.startupSessionId = "__new__";
      this.reportUiError(error, "Failed to load sessions");
    }
  }

  private async onAddProjectFromPathPrompt(): Promise<void> {
    const path = window.prompt("Enter project folder path");
    if (!path) {
      return;
    }
    try {
      const entry = await projectAddRecent(path);
      await this.loadStartupProjects();
      this.startupProjectPath = entry.path;
      await this.loadStartupSessions(entry.path);
    } catch (error) {
      this.reportUiError(error, "Invalid project path");
    }
  }

  private async startWorkspace(agent: string): Promise<void> {
    if (!this.startupProjectPath || this.startupBusy) {
      return;
    }
    this.startupBusy = true;
    try {
      const context = await workspaceInit(
        this.startupProjectPath,
        this.startupSessionId === "__new__" ? undefined : this.startupSessionId
      );
      this.workspaceContext = context;
      this.switcherProjectPath = context.project_path ?? "";
      this.switcherSessionId = context.session_id ?? "__new__";
      this.launchAgentName = agent;
      await this.resizeWindow(1280, 820);
      await this.loadSwitcherSessions();
      if (this.bootstrap?.panes.length === 0) {
        await this.quickStartAdd(agent);
      }
    } catch (error) {
      this.reportUiError(error, "Failed to initialize workspace");
    } finally {
      this.startupBusy = false;
    }
  }

  private async loadSwitcherSessions(): Promise<void> {
    if (!this.switcherProjectPath) {
      this.switcherSessions = [];
      this.switcherSessionId = "__new__";
      return;
    }
    try {
      this.switcherSessions = await sessionsList(this.switcherProjectPath);
      if (
        this.switcherSessionId !== "__new__" &&
        !this.switcherSessions.some((session) => session.id === this.switcherSessionId)
      ) {
        this.switcherSessionId = this.switcherSessions[0]?.id ?? "__new__";
      }
    } catch (error) {
      this.switcherSessions = [];
      this.reportUiError(error, "Failed to load workspace sessions");
    }
  }

  private async onSwitcherProjectSelect(path: string): Promise<void> {
    this.switcherProjectPath = path;
    this.switcherSessionId = "__new__";
    await this.loadSwitcherSessions();
  }

  private async applyWorkspaceSwitcher(): Promise<void> {
    if (!this.switcherProjectPath) {
      return;
    }
    try {
      const context = await workspaceInit(
        this.switcherProjectPath,
        this.switcherSessionId === "__new__" ? undefined : this.switcherSessionId
      );
      this.workspaceContext = context;
      this.switcherOpen = false;
      this.lastSubmitMessage = "Workspace context updated for new panels";
    } catch (error) {
      this.reportUiError(error, "Failed to update workspace");
    }
  }

  private projectNameForPath(path: string | undefined): string {
    if (!path) {
      return "-";
    }
    const match = this.startupProjects.find((project) => project.path === path);
    if (match) {
      return match.name;
    }
    const chunks = path.split("/").filter(Boolean);
    return chunks[chunks.length - 1] ?? path;
  }

  private filteredPaletteItems(): PaletteItem[] {
    const q = this.paletteQuery.trim().toLowerCase();
    if (!q) {
      return this.paletteItems;
    }
    return this.paletteItems.filter((item) => {
      return item.command.toLowerCase().includes(q) || item.description.toLowerCase().includes(q);
    });
  }

  private onPaletteQueryChange(event: CustomEvent<{ query: string }>): void {
    this.paletteQuery = event.detail.query;
    const filtered = this.filteredPaletteItems();
    if (this.paletteSelectedIndex >= filtered.length) {
      this.paletteSelectedIndex = 0;
    }
  }

  private onPaletteNav(event: CustomEvent<{ direction: "up" | "down" }>): void {
    const items = this.filteredPaletteItems();
    if (!items.length) {
      return;
    }
    if (event.detail.direction === "down") {
      this.paletteSelectedIndex = (this.paletteSelectedIndex + 1) % items.length;
    } else {
      this.paletteSelectedIndex = (this.paletteSelectedIndex - 1 + items.length) % items.length;
    }
  }

  private async onPaletteSelect(event: CustomEvent<{ item: PaletteItem }>): Promise<void> {
    if (!this.bootstrap) {
      return;
    }
    const item = event.detail.item;
    this.paletteOpen = false;
    this.paletteQuery = "";
    this.paletteSelectedIndex = 0;

    if (item.command.startsWith("/upload")) {
      this.inputValue = "/upload ";
      this.lastSubmitMessage = "Enter a host file path after /upload";
      return;
    }

    try {
      await inputSubmit(this.bootstrap.layout.focused_pane, item.command);
    } catch (error) {
      this.reportUiError(error, "Command Failed");
    }
  }

  private async onPaneFocusRequest(event: CustomEvent<{ paneId: number }>): Promise<void> {
    if (!this.bootstrap) {
      return;
    }

    try {
      const paneId = event.detail.paneId;
      await paneFocus(paneId);
    } catch (error) {
      this.reportUiError(error, "Focus Failed");
    }
  }

  private async onTabSelect(paneId: number): Promise<void> {
    if (!this.bootstrap) {
      return;
    }

      this.bootstrap = {
        ...this.bootstrap,
        panes: this.bootstrap.panes.map((pane) => ({
        ...pane,
        focused: pane.pane_id === paneId
      })),
      layout: {
        ...this.bootstrap.layout,
          focused_pane: paneId
        }
      };
      this.selectedSandboxPaneId = paneId;

    try {
      const pane = this.bootstrap.panes.find((item) => item.pane_id === paneId);
      if (pane && !pane.visible) {
        await paneOpen(paneId);
      }
      await paneFocus(paneId);
      this.focusPaneTerminal(paneId);
    } catch (error) {
      this.reportUiError(error, "Tab Select Failed");
    }
  }

  private async onPaneHideRequest(event: CustomEvent<{ paneId: number }>): Promise<void> {
    try {
      await paneClose(event.detail.paneId);
    } catch (error) {
      this.reportUiError(error, "Hide Failed");
    }
  }

  private selectSandboxPane(paneId: number): void {
    this.selectedSandboxPaneId = paneId;
  }

  private async onSandboxFocusRequest(event: CustomEvent<{ paneId: number }>): Promise<void> {
    const paneId = event.detail.paneId;
    this.selectSandboxPane(paneId);
    await this.onPaneFocusRequest(new CustomEvent("pane-focus-request", { detail: { paneId } }));
  }

  private async onSandboxOpenRequest(event: CustomEvent<{ paneId: number }>): Promise<void> {
    const paneId = event.detail.paneId;
    this.selectSandboxPane(paneId);
    try {
      await paneOpen(paneId);
      await paneFocus(paneId);
      this.focusPaneTerminal(paneId);
    } catch (error) {
      this.reportUiError(error, "Open Failed");
    }
  }

  private async onSandboxCloseRequest(event: CustomEvent<{ paneId: number }>): Promise<void> {
    this.selectSandboxPane(event.detail.paneId);
    await this.onPaneHideRequest(new CustomEvent("pane-hide-request", { detail: { paneId: event.detail.paneId } }));
  }

  private onSandboxKillRequest(event: CustomEvent<{ paneId: number }>): void {
    this.selectSandboxPane(event.detail.paneId);
    this.requestPaneKill(event.detail.paneId);
  }

  private async onSandboxZoomRequest(event: CustomEvent<{ paneId: number }>): Promise<void> {
    this.selectSandboxPane(event.detail.paneId);
    await this.onPaneZoomToggle(new CustomEvent("pane-zoom-toggle", { detail: { paneId: event.detail.paneId } }));
  }

  private async onSandboxCommand(event: CustomEvent<{ paneId?: number; command: string }>): Promise<void> {
    if (!this.bootstrap) {
      return;
    }
    const command = event.detail.command?.trim();
    if (!command) {
      return;
    }
    const paneId = event.detail.paneId ?? this.selectedSandboxPaneId ?? this.bootstrap.layout.focused_pane;
    this.selectSandboxPane(paneId);
    try {
      const pane = this.bootstrap.panes.find((item) => item.pane_id === paneId);
      if (pane && !pane.visible) {
        await paneOpen(paneId);
      }
      await paneFocus(paneId);
      const result = await inputSubmit(paneId, command);
      this.lastSubmitMessage = result.message ?? `Executed ${command}`;
      this.focusPaneTerminal(paneId);
    } catch (error) {
      this.reportUiError(error, "Sandbox Command Failed");
    }
  }

  private requestPaneKill(paneId: number): void {
    const pane = this.bootstrap?.panes.find((item) => item.pane_id === paneId);
    const paneLabel = pane ? `${pane.agent_name} · ${pane.sandbox_id_short}` : `panel ${paneId}`;
    this.activePopup = {
      type: "popup_enqueue",
      popup_id: `local-pane-kill-${paneId}`,
      kind: "warning",
      title: "Kill panel?",
      body: `Stop and remove ${paneLabel}? This cannot be undone.`,
      actions: [
        {
          id: `local-pane-kill-cancel:${paneId}`,
          label: "Cancel"
        },
        {
          id: `local-pane-kill-confirm:${paneId}`,
          label: "Kill",
          primary: true
        }
      ]
    };
  }

  private async onPaneZoomToggle(event: CustomEvent<{ paneId: number }>): Promise<void> {
    try {
      const paneId = event.detail.paneId;
      const layout = await paneZoomToggle(paneId);
      this.zoomedPaneId = layout.zoomed_pane ?? null;
      if (this.bootstrap) {
        this.bootstrap = {
          ...this.bootstrap,
          layout
        };
      }
    } catch (error) {
      this.reportUiError(error, "Zoom Failed");
    }
  }

  private async onSubmit(event: Event): Promise<void> {
    event.preventDefault();
    if (!this.bootstrap) {
      return;
    }

    try {
      const text = this.inputValue;
      const result = await inputSubmit(this.bootstrap.layout.focused_pane, text);
      this.inputValue = "";
      this.commandSuggestions = [];
      this.commandSuggestionIndex = 0;
      this.commandHistoryIndex = -1;
      this.commandHistoryDraft = "";
      this.pushCommandHistory(text);

      if (result.kind === "command_error") {
        this.lastSubmitMessage = result.message ?? "Command error";
        return;
      }

      if (result.kind === "command") {
        if (result.command_name === "/copy" || result.command_name === "copy") {
          const paneEl = this.querySelector<TerminalPaneElement>(
            `nsb-terminal-pane[pane-id="${this.bootstrap.layout.focused_pane}"]`
          );
          const copied = paneEl?.copySelectionOrVisibleBuffer?.() ?? "";
          if (copied.trim().length > 0) {
            await navigator.clipboard.writeText(copied);
            this.onPaneCopy(new CustomEvent("pane-copy", { detail: { paneId: this.bootstrap.layout.focused_pane } }));
            this.lastSubmitMessage = "Copied focused panel output";
            return;
          }
          this.lastSubmitMessage = "Nothing to copy from focused panel";
          return;
        }
        if (result.command_name === "/clearhistory" || result.command_name === "clearhistory") {
          this.clearCommandHistory();
        }
        this.lastSubmitMessage = result.message ?? `Executed ${result.command_name ?? "command"}`;
        return;
      }

      if (result.kind === "message") {
        this.lastSubmitMessage = result.message ?? "Message queued for focused panel";
        return;
      }

      this.lastSubmitMessage = "";
    } catch (error) {
      this.reportUiError(error, "Submit Failed");
    }
  }

  private async onUploadClick(): Promise<void> {
    if (!this.bootstrap) {
      return;
    }
    this.inputValue = "/upload ";
    this.lastSubmitMessage = "Enter a host file path after /upload";
  }

  private async onPasteImageClick(): Promise<void> {
    if (!this.bootstrap) {
      return;
    }
    try {
      await uploadPasteImage(this.bootstrap.layout.focused_pane);
    } catch (error) {
      this.reportUiError(error, "Paste Image Failed");
    }
  }

  private async onPopupDismiss(event: CustomEvent<{ popupId: string }>): Promise<void> {
    if (event.detail.popupId.startsWith("local-pane-kill-")) {
      this.activePopup = null;
      return;
    }
    try {
      await popupDismiss(event.detail.popupId);
      this.activePopup = null;
    } catch (error) {
      this.reportUiError(error, "Popup Dismiss Failed");
    }
  }

  private async onPopupAction(event: CustomEvent<{ popupId: string; actionId: string }>): Promise<void> {
    if (event.detail.actionId.startsWith("local-pane-kill-cancel:")) {
      this.activePopup = null;
      return;
    }

    if (event.detail.actionId.startsWith("local-pane-kill-confirm:")) {
      const paneId = Number.parseInt(event.detail.actionId.split(":")[1] ?? "", 10);
      if (Number.isFinite(paneId)) {
        try {
          await paneKill(paneId);
          if (this.bootstrap) {
            this.focusPaneTerminal(this.bootstrap.layout.focused_pane);
          }
        } catch (error) {
          this.reportUiError(error, "Kill Failed");
          return;
        }
      }
      this.activePopup = null;
      return;
    }

    try {
      await popupAction(event.detail.popupId, event.detail.actionId);
      this.activePopup = null;
    } catch (error) {
      this.reportUiError(error, "Popup Action Failed");
    }
  }

  private toggleSettingsPanel(): void {
    this.settingsOpen = !this.settingsOpen;
    if (this.settingsOpen) {
      this.sandboxPanelOpen = false;
    }
  }

  private toggleSandboxPanel(): void {
    this.sandboxPanelOpen = !this.sandboxPanelOpen;
    if (this.sandboxPanelOpen) {
      this.settingsOpen = false;
      if (this.bootstrap) {
        this.selectedSandboxPaneId = this.bootstrap.layout.focused_pane;
      }
    }
  }

  private async onThemeSelect(event: Event): Promise<void> {
    if (!this.bootstrap) {
      return;
    }
    try {
      const name = (event.target as HTMLSelectElement).value;
      const theme = await themeSet(name);
      document.documentElement.dataset.theme = theme.mode;
      this.bootstrap = {
        ...this.bootstrap,
        theme
      };
    } catch (error) {
      this.reportUiError(error, "Theme Switch Failed");
    }
  }

  private async refreshCommandSuggestions(partial: string): Promise<void> {
    if (!partial.startsWith("/")) {
      this.commandSuggestions = [];
      this.commandSuggestionIndex = 0;
      return;
    }

    const reqId = ++this.commandSuggestReqId;
    try {
      const suggestions = await commandAutocomplete(partial);
      if (reqId !== this.commandSuggestReqId) {
        return;
      }
      this.commandSuggestions = suggestions.slice(0, 8);
      this.commandSuggestionIndex = 0;
    } catch {
      if (reqId !== this.commandSuggestReqId) {
        return;
      }
      this.commandSuggestions = [];
      this.commandSuggestionIndex = 0;
    }
  }

  private applySelectedSuggestion(): void {
    const suggestion = this.commandSuggestions[this.commandSuggestionIndex];
    if (!suggestion) {
      return;
    }
    this.inputValue = `${suggestion} `;
    this.commandSuggestions = [];
    this.commandSuggestionIndex = 0;
  }

  private onInputKeyDown(event: KeyboardEvent): void {
    if (!this.commandSuggestions.length) {
      if (event.key === "ArrowUp") {
        if (!this.commandHistory.length) {
          return;
        }
        event.preventDefault();
        if (this.commandHistoryIndex === -1) {
          this.commandHistoryDraft = this.inputValue;
          this.commandHistoryIndex = 0;
        } else if (this.commandHistoryIndex < this.commandHistory.length - 1) {
          this.commandHistoryIndex += 1;
        }
        this.inputValue = this.commandHistory[this.commandHistoryIndex] ?? this.inputValue;
        return;
      }
      if (event.key === "ArrowDown" && this.commandHistoryIndex >= 0) {
        event.preventDefault();
        if (this.commandHistoryIndex === 0) {
          this.commandHistoryIndex = -1;
          this.inputValue = this.commandHistoryDraft;
          this.commandHistoryDraft = "";
          return;
        }
        this.commandHistoryIndex -= 1;
        this.inputValue = this.commandHistory[this.commandHistoryIndex] ?? this.inputValue;
      }
      return;
    }

    if (event.key === "ArrowDown") {
      event.preventDefault();
      this.commandSuggestionIndex = (this.commandSuggestionIndex + 1) % this.commandSuggestions.length;
      return;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      this.commandSuggestionIndex =
        (this.commandSuggestionIndex - 1 + this.commandSuggestions.length) % this.commandSuggestions.length;
      return;
    }
    if (event.key === "Tab") {
      event.preventDefault();
      this.applySelectedSuggestion();
    }
  }

  private onInputValueChange(event: Event): void {
    this.inputValue = (event.target as HTMLInputElement).value;
    void this.refreshCommandSuggestions(this.inputValue.trim());
  }

  private async quickStartAdd(agent: string): Promise<void> {
    if (!this.bootstrap) {
      return;
    }
    try {
      const paneId = this.bootstrap.layout.focused_pane;
      const projectArg = this.workspaceContext?.project_path
        ? ` --project "${this.workspaceContext.project_path.replace(/"/g, '\\"')}"`
        : "";
      const result = await inputSubmit(paneId, `/add ${agent}${projectArg}`);
      this.lastSubmitMessage = result.message ?? `Starting ${agent}...`;
    } catch (error) {
      this.reportUiError(error, "Quick Start Failed");
    }
  }

  render() {
    if (this.loadError) {
      return html`
        <div class="nsb-app">
          <div class="nsb-popup-scrim" style="display: flex; align-items: center; justify-content: center;">
            <div class="nsb-popup" style="display: block; max-width: 640px;">
              <div class="nsb-popup__header">
                <span class="nsb-popup__icon nsb-popup__icon--error">✗</span>
                <span class="nsb-popup__title">Desktop bootstrap failed</span>
              </div>
              <div class="nsb-popup__body">${this.loadError}</div>
            </div>
          </div>
        </div>
      `;
    }

    if (!this.bootstrap) {
      return html`<div class="nsb-app"><div class="welcome"><div class="welcome__tagline">Loading desktop shell...</div></div></div>`;
    }

    if (!this.isWorkspaceReady()) {
      return html`
        <div class="nsb-start-screen">
          <div class="nsb-start-screen__inner">
            <img class="nsb-start-screen__logo" src="/logo.png" alt="NSB logo" />
            <div class="nsb-start-screen__pickers">
              <div class="nsb-picker-panel">
                <div class="nsb-picker-panel__header">
                  <span>Projects</span>
                  <span class="nsb-picker-badge nsb-picker-badge--muted">${this.startupProjects.length} recent</span>
                </div>
                <div class="nsb-picker-panel__list">
                  ${this.startupProjects.map(
                    (project) => html`
                      <button
                        type="button"
                        class="nsb-picker-item ${this.startupProjectPath === project.path ? "is-selected" : ""}"
                        @click=${async () => {
                          this.startupProjectPath = project.path;
                          await this.loadStartupSessions(project.path);
                        }}
                      >
                        <div class="nsb-picker-item__title">${project.name}</div>
                        <div class="nsb-picker-item__path">${project.path}</div>
                        <div class="nsb-picker-item__meta">${project.last_opened}</div>
                      </button>
                    `
                  )}
                  ${this.startupProjects.length
                    ? null
                    : html`<div class="nsb-picker-panel__empty">No recent projects yet. Add one to continue.</div>`}
                </div>
                <div class="nsb-picker-panel__footer">
                  <button type="button" class="nsb-btn nsb-btn--ghost" style="width:100%;" @click=${() => void this.onAddProjectFromPathPrompt()}>
                    Open folder...
                  </button>
                </div>
              </div>

              <div class="nsb-picker-panel">
                <div class="nsb-picker-panel__header">
                  <span>Sessions</span>
                  <span class="nsb-picker-badge nsb-picker-badge--muted">${this.startupSessions.length ? `${this.startupSessions.length} saved` : "fresh"}</span>
                </div>
                <div class="nsb-picker-panel__list">
                  <button
                    type="button"
                    class="nsb-picker-item ${this.startupSessionId === "__new__" ? "is-selected" : ""}"
                    @click=${() => {
                      this.startupSessionId = "__new__";
                    }}
                  >
                    <div class="nsb-picker-item__title">Fresh workspace</div>
                    <div class="nsb-picker-item__meta">Start with a clean workspace</div>
                  </button>
                  ${this.startupSessions.map(
                    (session) => html`
                      <button
                        type="button"
                        class="nsb-picker-item ${this.startupSessionId === session.id ? "is-selected" : ""}"
                        @click=${() => {
                          this.startupSessionId = session.id;
                        }}
                      >
                        <div class="nsb-picker-item__title">${session.id}</div>
                        <div class="nsb-picker-item__meta">${session.panels} panels • ${session.updated}</div>
                        <div class="nsb-picker-item__path">${session.summary}</div>
                      </button>
                    `
                  )}
                </div>
              </div>
            </div>
            <div class="nsb-start-screen__agents ${this.startupProjectPath ? "" : "is-disabled"}">
              <button type="button" class="nsb-btn nsb-btn--primary" ?disabled=${!this.startupProjectPath || this.startupBusy} @click=${() => void this.startWorkspace("claude")}>/add claude</button>
              <button type="button" class="nsb-btn nsb-btn--primary" ?disabled=${!this.startupProjectPath || this.startupBusy} @click=${() => void this.startWorkspace("codex")}>/add codex</button>
              <button type="button" class="nsb-btn nsb-btn--primary" ?disabled=${!this.startupProjectPath || this.startupBusy} @click=${() => void this.startWorkspace("cursor")}>/add cursor</button>
            </div>
          </div>
        </div>
      `;
    }

    const paletteItems = this.filteredPaletteItems();
    const tabs = this.bootstrap.panes;

    if (tabs.length === 0) {
      return html`
        <div class="nsb-shell nsb-shell--empty">
          <header class="nsb-toolbar">
            <div class="nsb-toolbar__tabs"></div>
            <div class="nsb-toolbar__actions">
              <div class="nsb-workspace-selectors">
                <button type="button" class="nsb-workspace-select nsb-workspace-select--project" @click=${() => {
                  this.switcherOpen = !this.switcherOpen;
                }}>
                  <span class="nsb-workspace-select__label">Project: ${this.projectNameForPath(this.workspaceContext?.project_path)}</span>
                  <span class="nsb-workspace-select__chev">▾</span>
                </button>
                <button type="button" class="nsb-workspace-select nsb-workspace-select--session" @click=${() => {
                  this.switcherOpen = !this.switcherOpen;
                }}>
                  <span class="nsb-workspace-select__label">Session: ${this.workspaceContext?.session_id ?? "fresh"}</span>
                  <span class="nsb-workspace-select__chev">▾</span>
                </button>
              </div>
            </div>
          </header>
          <div class="nsb-app nsb-empty-state">
            <div class="nsb-launcher">
              <div class="nsb-launcher__title">Choose an agent to start this workspace</div>
              <div class="nsb-launcher__actions">
                <button type="button" class="nsb-btn nsb-btn--primary" @click=${() => void this.quickStartAdd("claude")}>/add claude</button>
                <button type="button" class="nsb-btn nsb-btn--primary" @click=${() => void this.quickStartAdd("codex")}>/add codex</button>
                <button type="button" class="nsb-btn nsb-btn--primary" @click=${() => void this.quickStartAdd("cursor")}>/add cursor</button>
              </div>
            </div>
          </div>
        </div>
      `;
    }

    return html`
      <div
        class="nsb-shell ${this.settingsOpen ? "nsb-shell--settings-open" : ""} ${this.sandboxPanelOpen
          ? "nsb-shell--sandbox-open"
          : ""}"
      >
        <header class="nsb-toolbar">
          <div class="nsb-toolbar__tabs">
            ${tabs.map(
              (pane) => html`
                <div class="nsb-tab ${pane.focused ? "is-active" : ""} ${pane.visible ? "" : "is-hidden"}">
                  <button type="button" class="nsb-tab__select" @click=${() => void this.onTabSelect(pane.pane_id)}>
                    <span
                      class="nsb-tab__dot ${pane.status === "connected"
                        ? "nsb-dot--connected"
                        : pane.status === "loading"
                          ? "nsb-dot--loading"
                          : "nsb-dot--error"}"
                    ></span>
                    <span>${pane.agent_name} · ${pane.sandbox_id_short}</span>
                    ${pane.visible ? null : html`<span class="nsb-tab__meta">hidden</span>`}
                  </button>
                  <button
                    type="button"
                    class="nsb-tab__close"
                    title="Kill panel"
                    @click=${(event: Event) => {
                      event.stopPropagation();
                      this.requestPaneKill(pane.pane_id);
                    }}
                  >
                    ×
                  </button>
                </div>
              `
            )}
          </div>
          <div class="nsb-toolbar__actions">
            <div class="nsb-workspace-selectors">
              <button
                type="button"
                class="nsb-workspace-select nsb-workspace-select--project"
                @click=${async () => {
                  this.switcherOpen = !this.switcherOpen;
                  if (this.switcherProjectPath) {
                    await this.loadSwitcherSessions();
                  }
                }}
              >
                <span class="nsb-workspace-select__label">Project: ${this.projectNameForPath(this.workspaceContext?.project_path)}</span>
                <span class="nsb-workspace-select__chev">▾</span>
              </button>
              <button
                type="button"
                class="nsb-workspace-select nsb-workspace-select--session"
                @click=${async () => {
                  this.switcherOpen = !this.switcherOpen;
                  if (this.switcherProjectPath) {
                    await this.loadSwitcherSessions();
                  }
                }}
              >
                <span class="nsb-workspace-select__label">Session: ${this.workspaceContext?.session_id ?? "fresh"}</span>
                <span class="nsb-workspace-select__chev">▾</span>
              </button>
              <div class="nsb-workspace-popover ${this.switcherOpen ? "is-open" : ""}">
                <div class="nsb-workspace-popover__header">Switch Project and Session</div>
                <div class="nsb-workspace-popover__body">
                  <div class="nsb-workspace-section__label">Project</div>
                  <div class="nsb-picker-panel__list" style="padding:0;">
                    ${this.startupProjects.map(
                      (project) => html`
                        <button
                          type="button"
                          class="nsb-picker-item ${this.switcherProjectPath === project.path ? "is-selected" : ""}"
                          @click=${() => void this.onSwitcherProjectSelect(project.path)}
                        >
                          <div class="nsb-picker-item__title">${project.name}</div>
                          <div class="nsb-picker-item__path">${project.path}</div>
                        </button>
                      `
                    )}
                  </div>
                  <div class="nsb-workspace-section__label">Session</div>
                  <div class="nsb-picker-panel__list" style="padding:0;">
                    <button
                      type="button"
                      class="nsb-picker-item ${this.switcherSessionId === "__new__" ? "is-selected" : ""}"
                      @click=${() => {
                        this.switcherSessionId = "__new__";
                      }}
                    >
                      <div class="nsb-picker-item__title">Fresh workspace</div>
                    </button>
                    ${this.switcherSessions.map(
                      (session) => html`
                        <button
                          type="button"
                          class="nsb-picker-item ${this.switcherSessionId === session.id ? "is-selected" : ""}"
                          @click=${() => {
                            this.switcherSessionId = session.id;
                          }}
                        >
                          <div class="nsb-picker-item__title">${session.id}</div>
                          <div class="nsb-picker-item__meta">${session.panels} panels • ${session.updated}</div>
                        </button>
                      `
                    )}
                  </div>
                </div>
                <div class="nsb-workspace-popover__footer">
                  <button type="button" class="nsb-btn nsb-btn--ghost" @click=${() => {
                    this.switcherOpen = false;
                  }}>Cancel</button>
                  <button type="button" class="nsb-btn nsb-btn--primary" @click=${() => void this.applyWorkspaceSwitcher()}>Apply</button>
                </div>
              </div>
            </div>
            <button
              class="nsb-toolbar__panel-btn ${this.sandboxPanelOpen ? "is-active" : ""}"
              title="Sandboxes"
              aria-label="Toggle sandboxes panel"
              @click=${this.toggleSandboxPanel}
            >
              ≡
            </button>
            <button class="nsb-toolbar__settings-btn" title="Settings" aria-label="Open settings" @click=${this.toggleSettingsPanel}>⚙</button>
          </div>
        </header>

        <div class="nsb-app">
          <nsb-pane-grid
            .panes=${this.bootstrap.panes}
            .rows=${this.bootstrap.layout.rows}
            .cols=${this.bootstrap.layout.cols}
            .zoomedPaneId=${this.zoomedPaneId}
            @pane-focus-request=${(event: CustomEvent<{ paneId: number }>) => void this.onPaneFocusRequest(event)}
            @pane-zoom-toggle=${(event: CustomEvent<{ paneId: number }>) => void this.onPaneZoomToggle(event)}
            @pane-hide-request=${(event: CustomEvent<{ paneId: number }>) => void this.onPaneHideRequest(event)}
            @pane-copy=${(event: CustomEvent<{ paneId: number }>) => this.onPaneCopy(event)}
          ></nsb-pane-grid>

          <form class="nsb-input-bar" @submit=${this.onSubmit}>
            ${this.commandSuggestions.length
              ? html`
                  <div class="nsb-command-suggest">
                    <div class="nsb-command-suggest__title">Commands</div>
                    ${this.commandSuggestions.map(
                      (cmd, idx) => html`
                        <button
                          type="button"
                          class="nsb-command-suggest__item ${idx === this.commandSuggestionIndex ? "is-active" : ""}"
                          @click=${() => {
                            this.commandSuggestionIndex = idx;
                            this.applySelectedSuggestion();
                          }}
                        >
                          ${cmd}
                        </button>
                      `
                    )}
                  </div>
                `
              : null}
            <div class="nsb-input-bar__mode-badge">panel ${this.bootstrap.layout.focused_pane}</div>
            <span class="nsb-input-bar__prompt">&gt;&gt;</span>
            <input
              class="nsb-input-bar__input"
              .value=${this.inputValue}
              @input=${this.onInputValueChange}
              @keydown=${this.onInputKeyDown}
              placeholder="Type /help or any prompt"
            />
            <span class="nsb-input-bar__hint" style="display:flex;align-items:center;gap:6px;">
              <button type="button" class="nsb-btn nsb-btn--ghost" style="padding:2px 8px;" @click=${this.onUploadClick}>upload</button>
              <button type="button" class="nsb-btn nsb-btn--ghost" style="padding:2px 8px;" @click=${this.onPasteImageClick}>paste</button>
            </span>
          </form>

          <div class="nsb-status-bar">
            <div class="nsb-status-bar__left">
              <div class="nsb-status-item nsb-status-item--accent">nanosb desktop ${this.bootstrap.version}</div>
              <div class="nsb-status-bar__sep"></div>
              <div class="nsb-status-item">${this.bootstrap.panes.length} panels</div>
            </div>
            <div class="nsb-status-bar__right">
              ${this.statusLine
                ? html`<div class="nsb-status-item" style="color: var(--text-subtle)">${this.statusLine}</div>`
                : this.lastSubmitMessage
                  ? html`<div class="nsb-status-item" style="color: var(--accent)">${this.lastSubmitMessage}</div>`
                  : html`<div class="nsb-status-item"><kbd class="nsb-kbd">Ctrl+P</kbd> palette</div>`}
            </div>
          </div>
        </div>

        <aside class="nsb-settings-panel ${this.settingsOpen ? "is-open" : ""}">
          <div class="nsb-settings-panel__header">
            <span>Settings</span>
            <button class="nsb-settings-panel__close" @click=${this.toggleSettingsPanel}>✕</button>
          </div>
          <div class="nsb-settings-panel__body">
            <label class="nsb-settings-field">
              <span class="nsb-settings-field__label">Color Scheme</span>
              <select
                class="nsb-settings-field__select"
                .value=${this.bootstrap.theme.name}
                @change=${(event: Event) => void this.onThemeSelect(event)}
              >
                ${this.themeOptions.map((name) => html`<option value=${name}>${name}</option>`)}
              </select>
            </label>
            <div class="nsb-settings-field">
              <span class="nsb-settings-field__label">Log file</span>
              <div class="nsb-settings-field__readonly" title=${this.logsPathValue || "Unavailable"}>
                ${this.logsPathValue || "Unavailable"}
              </div>
            </div>
          </div>
        </aside>

        <nsb-sandbox-panel
          .open=${this.sandboxPanelOpen}
          .panes=${this.bootstrap.panes}
          .selectedPaneId=${this.selectedSandboxPaneId}
          .zoomedPaneId=${this.zoomedPaneId}
          .editors=${this.availableEditors}
          @sandbox-panel-close=${() => {
            this.sandboxPanelOpen = false;
          }}
          @sandbox-select=${(event: CustomEvent<{ paneId: number }>) => this.selectSandboxPane(event.detail.paneId)}
          @sandbox-focus=${(event: CustomEvent<{ paneId: number }>) => void this.onSandboxFocusRequest(event)}
          @sandbox-open=${(event: CustomEvent<{ paneId: number }>) => void this.onSandboxOpenRequest(event)}
          @sandbox-close=${(event: CustomEvent<{ paneId: number }>) => void this.onSandboxCloseRequest(event)}
          @sandbox-zoom=${(event: CustomEvent<{ paneId: number }>) => void this.onSandboxZoomRequest(event)}
          @sandbox-kill=${(event: CustomEvent<{ paneId: number }>) => this.onSandboxKillRequest(event)}
          @sandbox-command=${(event: CustomEvent<{ paneId?: number; command: string }>) => void this.onSandboxCommand(event)}
          @sandbox-refresh-editors=${() => void this.refreshEditors()}
        ></nsb-sandbox-panel>

        <div class="nsb-toast-host" aria-live="polite">
          ${this.copyToastMessage ? html`<div class="nsb-toast">${this.copyToastMessage}</div>` : null}
        </div>

        <nsb-command-palette
          .open=${this.paletteOpen}
          .query=${this.paletteQuery}
          .items=${paletteItems}
          .selectedIndex=${this.paletteSelectedIndex}
          @palette-query-change=${(event: CustomEvent<{ query: string }>) => this.onPaletteQueryChange(event)}
          @palette-nav=${(event: CustomEvent<{ direction: "up" | "down" }>) => this.onPaletteNav(event)}
          @palette-select=${(event: CustomEvent<{ item: PaletteItem }>) => void this.onPaletteSelect(event)}
          @palette-close=${() => {
            this.paletteOpen = false;
          }}
        ></nsb-command-palette>

        <nsb-popup-host
          .popup=${this.activePopup}
          @popup-dismiss=${(event: CustomEvent<{ popupId: string }>) => void this.onPopupDismiss(event)}
          @popup-action=${(event: CustomEvent<{ popupId: string; actionId: string }>) =>
            void this.onPopupAction(event)}
        ></nsb-popup-host>
      </div>
    `;
  }
}

if (!customElements.get("nsb-app-shell")) {
  customElements.define("nsb-app-shell", NsbAppShell);
}
