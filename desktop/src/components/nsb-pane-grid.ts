import { LitElement, html } from "lit";
import type { PaneSummary } from "../types/ipc";
import "./nsb-terminal-pane";

export class NsbPaneGrid extends LitElement {
  static properties = {
    panes: { type: Array },
    rows: { type: Number },
    cols: { type: Number },
    zoomedPaneId: { type: Number, attribute: "zoomed-pane-id" }
  };

  panes: PaneSummary[] = [];

  rows = 1;

  cols = 1;

  zoomedPaneId: number | null = null;

  protected createRenderRoot(): HTMLElement {
    return this;
  }

  private onPaneClick(paneId: number): void {
    this.dispatchEvent(
      new CustomEvent("pane-focus-request", {
        detail: { paneId },
        bubbles: true,
        composed: true
      })
    );
  }

  private onPaneZoomToggle(paneId: number): void {
    this.dispatchEvent(
      new CustomEvent("pane-zoom-toggle", {
        detail: { paneId },
        bubbles: true,
        composed: true
      })
    );
  }

  private onPaneHide(paneId: number): void {
    this.dispatchEvent(
      new CustomEvent("pane-hide-request", {
        detail: { paneId },
        bubbles: true,
        composed: true
      })
    );
  }

  render() {
    const visiblePanes = this.panes.filter((pane) => pane.visible);
    const rendered =
      this.zoomedPaneId == null
        ? visiblePanes
        : visiblePanes.filter((pane) => pane.pane_id === this.zoomedPaneId);
    const rowCount = this.zoomedPaneId == null ? Math.max(1, this.rows) : 1;
    const colCount = this.zoomedPaneId == null ? Math.max(1, this.cols) : 1;
    const layout = `${rowCount}x${colCount}`;
    const singlePanel = rendered.length === 1;
    const gridStyle = `grid-template-columns: repeat(${colCount}, minmax(0, 1fr)); grid-template-rows: repeat(${rowCount}, minmax(0, 1fr));`;

    return html`
      <div class="nsb-grid" data-layout="${layout}" style=${gridStyle}>
        ${rendered.map(
          (pane) => html`
            <div
              class="nsb-pane ${pane.focused ? "is-focused" : ""} ${singlePanel ? "is-maximized" : ""}"
              @click=${() => this.onPaneClick(pane.pane_id)}
            >
              <div class="nsb-pane__header">
                <div
                  class="nsb-dot ${pane.status === "connected"
                    ? "nsb-dot--connected"
                    : pane.status === "loading"
                      ? "nsb-dot--loading"
                      : "nsb-dot--error"}"
                ></div>
                <span class="nsb-pane__name">${pane.agent_name} · ${pane.sandbox_id_short}</span>
                <span class="nsb-pane__id">${pane.pane_id}</span>
                <span class="nsb-pane__time"
                  >${pane.elapsed_ms ? `${Math.floor(pane.elapsed_ms / 1000)}s` : "--"}</span
                >
                ${(() => {
                  const isZoomed = this.zoomedPaneId === pane.pane_id;
                  return html`
                    <div style="margin-left:auto;display:flex;gap:4px;">
                      <button
                        class="nsb-pane-btn"
                        title="Hide panel"
                        @click=${(event: Event) => {
                          event.stopPropagation();
                          this.onPaneHide(pane.pane_id);
                        }}
                      >
                        _
                      </button>
                      <button
                        class="nsb-pane-btn"
                        title=${isZoomed ? "Restore grid" : "Maximize panel"}
                        @click=${(event: Event) => {
                          event.stopPropagation();
                          this.onPaneZoomToggle(pane.pane_id);
                        }}
                      >
                        ${isZoomed ? "—" : "□"}
                      </button>
                    </div>
                  `;
                })()}
              </div>
              <div class="nsb-pane__terminal">
                <nsb-terminal-pane
                  pane-id="${pane.pane_id}"
                  sandbox-id="${pane.sandbox_id_short}"
                  mode="${pane.mode}"
                  loading-message="${pane.loading_message ?? "Starting sandbox..."}"
                ></nsb-terminal-pane>
              </div>
            </div>
          `
        )}
      </div>
    `;
  }
}

if (!customElements.get("nsb-pane-grid")) {
  customElements.define("nsb-pane-grid", NsbPaneGrid);
}
