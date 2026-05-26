# nanosb Desktop — HTML Mockup

Static HTML/CSS design prototype. No JavaScript frameworks, no Rust, no Tauri.
Open any page directly in a browser (Chrome / Safari / Firefox).

## Screens

| File | Description |
|------|-------------|
| `index.html` | Empty / welcome state |
| `grid.html` | 2×2 pane grid with mixed panel states |
| `palette.html` | Slash-command palette overlay |
| `popup.html` | System popups (info / success / warning / error) |
| `auth-browser.html` | In-app OAuth browser webview |

## How to view

```bash
# From this directory, open index.html in your browser:
open index.html          # macOS
xdg-open index.html      # Linux
start index.html         # Windows
```

Each page has a **Toggle theme** button (top-left) to switch between light and dark,
and a **nav panel** (top-right) to jump between screens.

## Stylesheet structure

```
styles/
  tokens.css        # CSS variables — single source of truth for all design tokens
  layout.css        # App shell, grid, pane sizing, scrollbar, focus ring
  components.css    # Pane chrome, input bar, status bar, palette, popups, buttons
```

All values in `tokens.css` map directly to the Tauri runtime `theme_get()` response
defined in `design/ipc.md`.
