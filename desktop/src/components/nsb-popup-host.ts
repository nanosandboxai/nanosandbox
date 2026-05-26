import { LitElement, html } from "lit";
import type { PopupAction, PopupEvent } from "../types/ipc";

export class NsbPopupHost extends LitElement {
  static properties = {
    popup: { type: Object }
  };

  popup: PopupEvent | null = null;

  protected createRenderRoot(): HTMLElement {
    return this;
  }

  private onDismiss(): void {
    if (!this.popup) {
      return;
    }
    this.dispatchEvent(
      new CustomEvent("popup-dismiss", {
        detail: { popupId: this.popup.popup_id },
        bubbles: true,
        composed: true
      })
    );
  }

  private onAction(action: PopupAction): void {
    if (!this.popup) {
      return;
    }
    this.dispatchEvent(
      new CustomEvent("popup-action", {
        detail: { popupId: this.popup.popup_id, actionId: action.id },
        bubbles: true,
        composed: true
      })
    );
  }

  render() {
    if (!this.popup) {
      return html``;
    }

    const icon =
      this.popup.kind === "success"
        ? "✓"
        : this.popup.kind === "warning"
          ? "⚠"
          : this.popup.kind === "error"
            ? "✗"
            : "i";

    return html`
      <div class="nsb-popup-scrim">
        <div class="nsb-popup" style="display:block;">
          <div class="nsb-popup__header">
            <span class="nsb-popup__icon">${icon}</span>
            <span class="nsb-popup__title">${this.popup.title}</span>
            <button class="nsb-popup__close" @click=${() => this.onDismiss()}>✕</button>
          </div>
          <div class="nsb-popup__body">${this.popup.body}</div>
          <div class="nsb-popup__footer">
            ${this.popup.actions.map(
              (action) => html`
                <button
                  class="nsb-btn ${action.primary ? "nsb-btn--primary" : "nsb-btn--ghost"}"
                  @click=${() => this.onAction(action)}
                >
                  ${action.label}
                </button>
              `
            )}
          </div>
        </div>
      </div>
    `;
  }
}

if (!customElements.get("nsb-popup-host")) {
  customElements.define("nsb-popup-host", NsbPopupHost);
}
