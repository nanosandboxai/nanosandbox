import { LitElement, html } from "lit";
import type { PaneSummary } from "../types/ipc";

type SectionKey = "git" | "mcp" | "skills" | "agent" | "env" | "editors";

export class NsbSandboxPanel extends LitElement {
  static properties = {
    open: { type: Boolean },
    panes: { type: Array },
    selectedPaneId: { type: Number, attribute: "selected-pane-id" },
    zoomedPaneId: { type: Number, attribute: "zoomed-pane-id" },
    editors: { type: Array },
    sectionState: { state: true }
  };

  open = false;

  panes: PaneSummary[] = [];

  selectedPaneId: number | null = null;

  zoomedPaneId: number | null = null;

  editors: string[] = [];

  private sectionState: Record<SectionKey, boolean> = {
    git: false,
    mcp: false,
    skills: false,
    agent: false,
    env: false,
    editors: false
  };

  protected createRenderRoot(): HTMLElement {
    return this;
  }

  private toggleSection(key: SectionKey): void {
    this.sectionState = {
      ...this.sectionState,
      [key]: !this.sectionState[key]
    };
  }

  private emit(name: string, detail: Record<string, unknown>): void {
    this.dispatchEvent(new CustomEvent(name, { detail }));
  }

  private selectedPane(): PaneSummary | null {
    return this.panes.find((pane) => pane.pane_id === this.selectedPaneId) ?? this.panes.find((pane) => pane.focused) ?? null;
  }

  render() {
    const pane = this.selectedPane();
    const hasPane = pane != null;
    return html`
      <aside class="nsb-sandbox-panel ${this.open ? "is-open" : ""}">
        <div class="nsb-sandbox-panel__header">
          <span>Sandboxes</span>
          <button type="button" class="nsb-settings-panel__close" @click=${() => this.emit("sandbox-panel-close", {})}>✕</button>
        </div>
        <div class="nsb-sandbox-panel__body">
          <section class="nsb-sandbox-section">
            <button type="button" class="nsb-sandbox-section__title" @click=${() => this.toggleSection("git")}>
              <span>${this.sectionState.git ? "▼" : "▶"} Git</span>
            </button>
            ${this.sectionState.git
              ? html`
                  <div class="nsb-sandbox-section__content nsb-sandbox-button-row">
                    <button type="button" ?disabled=${!hasPane} @click=${() => this.emit("sandbox-command", { paneId: pane?.pane_id, command: "/branches" })}>branches</button>
                    <button type="button" ?disabled=${!hasPane} @click=${() => this.emit("sandbox-command", { paneId: pane?.pane_id, command: "/gitsync" })}>status</button>
                    <button type="button" ?disabled=${!hasPane} @click=${() => this.emit("sandbox-command", { paneId: pane?.pane_id, command: "/gitsync on" })}>on</button>
                    <button type="button" ?disabled=${!hasPane} @click=${() => this.emit("sandbox-command", { paneId: pane?.pane_id, command: "/gitsync off" })}>off</button>
                    <button type="button" ?disabled=${!hasPane} @click=${() => this.emit("sandbox-command", { paneId: pane?.pane_id, command: "/gitsync now" })}>now</button>
                  </div>
                `
              : null}
          </section>

          <section class="nsb-sandbox-section">
            <button type="button" class="nsb-sandbox-section__title" @click=${() => this.toggleSection("mcp")}>
              <span>${this.sectionState.mcp ? "▼" : "▶"} MCP</span>
            </button>
            ${this.sectionState.mcp
              ? html`
                  <div class="nsb-sandbox-section__content nsb-sandbox-button-row">
                    <button type="button" ?disabled=${!hasPane} @click=${() => this.emit("sandbox-command", { paneId: pane?.pane_id, command: "/mcp list" })}>list</button>
                    <button type="button" ?disabled=${!hasPane} @click=${() => this.emit("sandbox-command", { paneId: pane?.pane_id, command: "/mcp" })}>toggle</button>
                  </div>
                `
              : null}
          </section>

          <section class="nsb-sandbox-section">
            <button type="button" class="nsb-sandbox-section__title" @click=${() => this.toggleSection("skills")}>
              <span>${this.sectionState.skills ? "▼" : "▶"} Skills</span>
            </button>
            ${this.sectionState.skills
              ? html`
                  <div class="nsb-sandbox-section__content nsb-sandbox-button-row">
                    <button type="button" ?disabled=${!hasPane} @click=${() => this.emit("sandbox-command", { paneId: pane?.pane_id, command: "/skills list" })}>list</button>
                  </div>
                `
              : null}
          </section>

          <section class="nsb-sandbox-section">
            <button type="button" class="nsb-sandbox-section__title" @click=${() => this.toggleSection("agent")}>
              <span>${this.sectionState.agent ? "▼" : "▶"} Agent</span>
            </button>
            ${this.sectionState.agent
              ? html`
                  <div class="nsb-sandbox-section__content nsb-sandbox-button-row">
                    <button type="button" ?disabled=${!hasPane} @click=${() => this.emit("sandbox-command", { paneId: pane?.pane_id, command: "/agent show" })}>show</button>
                    <button type="button" ?disabled=${!hasPane} @click=${() => this.emit("sandbox-command", { paneId: pane?.pane_id, command: "/agent list" })}>list</button>
                  </div>
                `
              : null}
          </section>

          <section class="nsb-sandbox-section">
            <button type="button" class="nsb-sandbox-section__title" @click=${() => this.toggleSection("env")}>
              <span>${this.sectionState.env ? "▼" : "▶"} Env</span>
            </button>
            ${this.sectionState.env
              ? html`
                  <div class="nsb-sandbox-section__content nsb-sandbox-button-row">
                    <button type="button" ?disabled=${!hasPane} @click=${() => this.emit("sandbox-command", { paneId: pane?.pane_id, command: "/env" })}>list</button>
                  </div>
                `
              : null}
          </section>

          <section class="nsb-sandbox-section">
            <button type="button" class="nsb-sandbox-section__title" @click=${() => this.toggleSection("editors")}>
              <span>${this.sectionState.editors ? "▼" : "▶"} Editors</span>
              <span>${this.editors.length}</span>
            </button>
            ${this.sectionState.editors
              ? html`
                  <div class="nsb-sandbox-section__content nsb-sandbox-button-row">
                    <button type="button" @click=${() => this.emit("sandbox-refresh-editors", {})}>refresh</button>
                    ${this.editors.map(
                      (editor) => html`
                        <button
                          type="button"
                          ?disabled=${!hasPane}
                          @click=${() => this.emit("sandbox-command", { paneId: pane?.pane_id, command: `/edit ${editor}` })}
                        >
                          ${editor}
                        </button>
                      `
                    )}
                  </div>
                `
              : null}
          </section>
        </div>
      </aside>
    `;
  }
}

if (!customElements.get("nsb-sandbox-panel")) {
  customElements.define("nsb-sandbox-panel", NsbSandboxPanel);
}
