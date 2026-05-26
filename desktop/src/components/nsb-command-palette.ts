import { LitElement, html } from "lit";

export interface PaletteItem {
  item_id: string;
  command: string;
  description: string;
  category?: string;
}

export class NsbCommandPalette extends LitElement {
  static properties = {
    open: { type: Boolean },
    query: { type: String },
    items: { type: Array },
    selectedIndex: { type: Number }
  };

  open = false;

  query = "";

  items: PaletteItem[] = [];

  selectedIndex = 0;

  protected createRenderRoot(): HTMLElement {
    return this;
  }

  private onInput(event: Event): void {
    const query = (event.target as HTMLInputElement).value;
    this.dispatchEvent(new CustomEvent("palette-query-change", { detail: { query } }));
  }

  private onKeyDown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      this.dispatchEvent(new CustomEvent("palette-close"));
      return;
    }

    if (event.key === "ArrowDown") {
      event.preventDefault();
      this.dispatchEvent(new CustomEvent("palette-nav", { detail: { direction: "down" } }));
      return;
    }

    if (event.key === "ArrowUp") {
      event.preventDefault();
      this.dispatchEvent(new CustomEvent("palette-nav", { detail: { direction: "up" } }));
      return;
    }

    if (event.key === "Enter") {
      event.preventDefault();
      const item = this.items[this.selectedIndex];
      if (!item) {
        return;
      }
      this.dispatchEvent(new CustomEvent("palette-select", { detail: { item } }));
    }
  }

  render() {
    if (!this.open) {
      return html``;
    }

    const groups = new Map<string, Array<{ item: PaletteItem; index: number }>>();
    this.items.forEach((item, index) => {
      const category = item.category ?? "General";
      const existing = groups.get(category);
      if (existing) {
        existing.push({ item, index });
      } else {
        groups.set(category, [{ item, index }]);
      }
    });

    return html`
      <div class="nsb-palette-scrim">
        <div class="nsb-palette">
          <div class="nsb-palette__input-row">
            <span style="font-family:var(--font-mono);font-size:14px;color:var(--accent);flex-shrink:0;">&gt;&gt;</span>
            <input
              class="nsb-palette__input"
              .value=${this.query}
              @input=${this.onInput}
              @keydown=${this.onKeyDown}
              autofocus
            />
            <kbd class="nsb-kbd">Esc</kbd>
          </div>

          <div class="nsb-palette__list">
            ${Array.from(groups.entries()).map(
              ([category, entries]) => html`
                <div class="nsb-palette__section-label">${category}</div>
                ${entries.map(
                  ({ item, index }) => html`
                    <div
                      class="nsb-palette__item ${index === this.selectedIndex ? "is-selected" : ""}"
                      @click=${() => this.dispatchEvent(new CustomEvent("palette-select", { detail: { item } }))}
                    >
                      <span class="nsb-palette__item-icon">/</span>
                      <span class="nsb-palette__item-name">${item.command}</span>
                      <span class="nsb-palette__item-desc">${item.description}</span>
                    </div>
                  `
                )}
              `
            )}
          </div>
        </div>
      </div>
    `;
  }
}

if (!customElements.get("nsb-command-palette")) {
  customElements.define("nsb-command-palette", NsbCommandPalette);
}
