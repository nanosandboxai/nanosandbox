import { LitElement, html } from "lit";
import type { PropertyValues } from "lit";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import { terminalResize, terminalWrite } from "../ipc/client";
import type { TerminalWriteFrame } from "../types/ipc";

export class NsbTerminalPane extends LitElement {
  static properties = {
    paneId: { type: Number, attribute: "pane-id" },
    sandboxId: { type: String, attribute: "sandbox-id" },
    mode: { type: String },
    loadingMessage: { type: String, attribute: "loading-message" }
  };

  paneId = -1;

  sandboxId = "";

  mode: "loading" | "terminal" | "headless" = "terminal";

  loadingMessage = "Starting sandbox...";

  private term: Terminal | null = null;

  private fit: FitAddon | null = null;

  private resizeObserver: ResizeObserver | null = null;

  private pendingResizeFrame = 0;

  private lastCols = 0;

  private lastRows = 0;

  private nextOutboundSeq = 0;

  private lastInboundSeq = 0;

  private mountEl: HTMLElement | null = null;

  private onCopy = (): void => {
    this.dispatchEvent(
      new CustomEvent("pane-copy", {
        detail: { paneId: this.paneId },
        bubbles: true,
        composed: true
      })
    );
  };

  protected createRenderRoot(): HTMLElement {
    return this;
  }

  disconnectedCallback(): void {
    super.disconnectedCallback();
    this.disposeTerminal();
  }

  protected updated(changedProperties: PropertyValues<this>): void {
    if (this.mode === "terminal") {
      if (this.term && (changedProperties.has("paneId") || changedProperties.has("sandboxId"))) {
        this.disposeTerminal();
      }
      this.ensureTerminal();
    } else {
      this.disposeTerminal();
    }
  }

  appendBase64Data(seq: number, base64Data: string): void {
    if (!this.term) {
      return;
    }
    if (seq <= this.lastInboundSeq) {
      return;
    }
    this.lastInboundSeq = seq;
    this.term.write(base64ToText(base64Data));
  }

  copySelectionOrVisibleBuffer(): string {
    if (!this.term) {
      return "";
    }
    const selection = this.term.getSelection();
    if (selection.trim().length > 0) {
      return selection;
    }
    const buffer = this.term.buffer.active;
    const lines: string[] = [];
    for (let i = 0; i < buffer.length; i += 1) {
      const line = buffer.getLine(i);
      if (!line) {
        continue;
      }
      lines.push(line.translateToString(true));
    }
    return lines.join("\n").trimEnd();
  }

  focusTerminal(): void {
    this.term?.focus();
  }

  private ensureTerminal(): void {
    if (this.term) {
      return;
    }

    const mount = this.querySelector<HTMLElement>(".nsb-xterm-root");
    if (!mount) {
      return;
    }

    const term = new Terminal({
      cursorBlink: true,
      fontSize: 13,
      fontFamily: '"JetBrains Mono", "Fira Code", ui-monospace, monospace',
      theme: {
        background: "#130c0c",
        foreground: "#f0eded",
        cursor: "#F05454",
        selectionBackground: "rgba(240,84,84,0.25)"
      }
    });

    const fit = new FitAddon();
    term.loadAddon(fit);

    try {
      term.loadAddon(new WebglAddon());
    } catch {
      // WebGL fallback is acceptable.
    }

    term.open(mount);
    mount.addEventListener("copy", this.onCopy);

    term.onData((input) => {
      this.nextOutboundSeq += 1;
      const frame: TerminalWriteFrame = {
        pane_id: this.paneId,
        seq: this.nextOutboundSeq,
        encoding: "base64",
        data: textToBase64(input)
      };
      void terminalWrite(frame);
    });

    this.term = term;
    this.fit = fit;
    this.mountEl = mount;
    this.fitAndNotifyResize();
    this.observeResize(mount);
  }

  private observeResize(mount: HTMLElement): void {
    this.resizeObserver?.disconnect();
    this.resizeObserver = new ResizeObserver(() => {
      this.scheduleFitAndResize();
    });
    this.resizeObserver.observe(mount);
  }

  private scheduleFitAndResize(): void {
    if (this.pendingResizeFrame) {
      cancelAnimationFrame(this.pendingResizeFrame);
    }
    this.pendingResizeFrame = requestAnimationFrame(() => {
      this.pendingResizeFrame = 0;
      this.fitAndNotifyResize();
    });
  }

  private fitAndNotifyResize(): void {
    if (!this.term || !this.fit) {
      return;
    }
    this.fit.fit();
    const { cols, rows } = this.term;
    if (cols === this.lastCols && rows === this.lastRows) {
      return;
    }
    this.lastCols = cols;
    this.lastRows = rows;
    void terminalResize(this.paneId, cols, rows);
  }

  private disposeTerminal(): void {
    if (this.pendingResizeFrame) {
      cancelAnimationFrame(this.pendingResizeFrame);
      this.pendingResizeFrame = 0;
    }
    if (this.resizeObserver) {
      this.resizeObserver.disconnect();
      this.resizeObserver = null;
    }
    if (this.term) {
      this.term.dispose();
    }
    if (this.mountEl) {
      this.mountEl.removeEventListener("copy", this.onCopy);
      this.mountEl = null;
    }
    this.term = null;
    this.fit = null;
    this.nextOutboundSeq = 0;
    this.lastInboundSeq = 0;
    this.lastCols = 0;
    this.lastRows = 0;
  }

  render() {
    if (this.mode !== "terminal") {
      return html`
        <div class="nsb-terminal-frame nsb-terminal-frame--loading">
          <div class="nsb-terminal-loading">
            <div class="nsb-terminal-loading__title">${this.loadingMessage}</div>
            <div class="nsb-terminal-loading__hint">Please wait while the session boots.</div>
          </div>
        </div>
      `;
    }

    return html`<div class="nsb-terminal-frame"><div class="nsb-xterm-root"></div></div>`;
  }
}

if (!customElements.get("nsb-terminal-pane")) {
  customElements.define("nsb-terminal-pane", NsbTerminalPane);
}

function textToBase64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (let i = 0; i < bytes.length; i += 1) {
    binary += String.fromCharCode(bytes[i]);
  }
  return btoa(binary);
}

function base64ToText(value: string): string {
  try {
    const binary = atob(value);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i += 1) {
      bytes[i] = binary.charCodeAt(i);
    }
    return new TextDecoder().decode(bytes);
  } catch {
    return "";
  }
}
