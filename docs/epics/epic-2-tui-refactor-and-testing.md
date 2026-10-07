# Epic 2 — TUI refactor, command audit, and testability

Status: draft (detailed)
Date: 2026-10-07
Related: PR #91 (TUI migrated to supervisor console), Epic 1

## 1. Context (verified 2026-10-07)

| File | Size | Tests |
|---|---|---|
| `src/tui/run.rs` | ~5.6k lines | 10 |
| `src/tui/commands.rs` | — | 81 |
| `src/tui/app.rs` | ~1.4k lines | 25 |
| `src/tui/renderer.rs` | ~1.6k lines | — |
| `src/tui/terminal.rs` | ~1.7k lines | — |
| `src/tui/upload.rs` | ~0.4k lines | — |

**Facts that shape the plan**
- `renderer.rs` exposes `pub fn render(frame: &mut Frame, app: &mut App)` — a clean
  headless entry point; `App` is constructible without a TTY. → `TestBackend`
  frame tests are straightforward today.
- The supervisor migration removed the gateway/SSH transports; several commands
  that used to mutate in-VM config now **print a “removed” notice** but are still
  parsed and listed in `/help` — misleading.
- The interactive attach path is compiled + unit-tested but **never verified on a
  real terminal**.

## 2. Command audit (all 34 `Command` variants)

| Command | Status | Action |
|---|---|---|
| `/quit` | supported | keep |
| `/destroy` | supported | keep |
| `/help` | supported | **generate from the enum** |
| `/clearhistory` | supported | keep |
| `/close` | supported | keep |
| `/open` | supported | keep |
| `/focus` | supported | keep |
| `/add` | supported (supervisor spawn) | keep |
| `/kill` | supported (stops supervisor) | keep |
| `/reconnect` | supported (console) | keep |
| `/sandboxes` | supported | keep |
| `/copy` | supported | keep |
| `/zoom` | supported | keep |
| `/env` | supported | keep |
| `/upload` | supported (virtiofs write) | keep |
| `/paste-image` | supported | keep |
| `/theme` | supported | keep |
| `/branches` | supported (host git) | keep |
| `/gitsync` | supported (host git) | keep |
| `/edit` | supported (host tool) | keep |
| `/mcp` (toggle), `/mcp list` | supported (read-only) | keep |
| `/skills list`, `/skills show` | supported (read-only) | keep |
| `/agent show`, `/agent list`, `/agent info` | supported (read-only) | keep |
| `/mcp add\|remove\|enable\|disable` | **deprecated stub** | **remove** parser+help |
| `/skills add\|remove` | **deprecated stub** | **remove** |
| `/agent set` | **deprecated stub** | **remove** |

**Totals:** 27 supported · 7 deprecated stubs to remove.

Rationale for removing (not hinting): the deploy-only model means agent/MCP/skills
config changes happen in `sandbox.yml` + `nanosb apply`. A single `/help` line
("config is declarative; edit sandbox.yml and run `nanosb apply`") replaces four
dead commands and removes ~7 dead handlers + tests.

## 3. Goal

1. Every advertised command **works**; nothing is a silent stub.
2. The event loop is **separable from the terminal**, so it can be driven headlessly.
3. A real test suite (frames + handlers + scripted VM) proves the TUI works,
   runnable locally and in CI.

Non-goals: new TUI features; visual redesign; adding metrics/logs panels (tracked
against Epic 1's surface later).

## 4. Testing strategy (detail)

### 4.1 Frame tests — `TestBackend` (no TTY, no VM)
Render `App` states into a `ratatui::backend::TestBackend` and assert on the
buffer. Concrete cases:
- welcome/branded screen (no panels)
- one panel loading (spinner + `loading_message`)
- one panel error (`loading_error` shown)
- one panel terminal mode with sample `TerminalData`
- multi-panel grid (2–4 panels) layout regions
- MCP sidebar open; sandbox sidebar open
- focused vs unfocused panel border
- help overlay contents
- status bar / system message popup
Assertions: check key substrings and that regions are non-empty; avoid full-buffer
snapshots unless using `insta` (see §8).

### 4.2 Handler/state tests (extend the existing 115)
Drive `handle_command(app, cmd)` and assert `App` state:
- `/add` → panel pushed, spawn task started (mock/observe tx event)
- `/close`, `/open`, `/focus` → visibility + focus transitions
- `/kill` → panel removed, supervisor stop requested
- `/env KEY=VALUE` → `panel.env` updated; bare `/env` lists
- `/theme` → `app.settings.ui.theme` changes
- `/zoom` → `app.zoomed` toggles
- `/reconnect` → panel enters reconnecting + console attach attempt
- removed commands no longer parse (see 4.3)

### 4.3 Parser tests (keep + extend the 81)
- Every removed command returns `ParseResult::NotACommand`.
- `/help` listing == the supported command set (a test asserts parity).
- Alias coverage (`/q`, `/quit`), bad args → error message.

### 4.4 Scripted VM end-to-end (no human, `#[ignore]`)
A test/example that boots a real supervised sandbox (alpine) and drives the TUI
event loop programmatically:
1. add a panel → assert `AppEvent::SupervisorReady` and `panel.backend == Supervisor`
2. attach console → assert `AppEvent::TerminalData` bytes arrive
3. `Command::Reconnect` → assert re-attach succeeds
4. `Command::Kill` → assert supervisor stops (`nanosb ps` shows it gone)
Run with `NANOSB_BINARY_PATH` + `--ignored`, like `next_mode_test`.

### 4.5 Local verification harness (manual, documented)
- A `scripts/tui-smoke.sh` that: builds, boots a supervised sandbox, launches the
  TUI under a **pty** (`script`/`expect`), sends a scripted key sequence, captures
  the screen, asserts markers.
- Documented as the "verify the TUI by hand" path for interactive attach (the one
  thing frame tests can't cover).

## 5. Workstreams

### WS1 — Command surface cleanup
- Remove `McpAdd/McpRemove/McpEnable/McpDisable`, `SkillsAdd/SkillsRemove`,
  `AgentSet` variants, their parse arms, handlers, and dead tests.
- Add one `/help` line describing the declarative-config model.
- **Generate `/help` from the supported set** (single source of truth).
- AC: `grep -r 'was removed' src/tui/**` returns nothing; help == supported set.

### WS2 — Headless event-loop extraction
- Split `run_tui` into: (a) terminal setup/teardown, (b) a headless `run_step` /
  `handle_event(app, ev, tx)` core.
- Add an event-injection seam so tests feed `CrosstermEvent`s and observe `App`.
- Keep the crossterm + alt-screen code isolated in one place.
- Do this **behind tests**, one handler group at a time (run.rs is large).
- AC: a test constructs `App`, feeds 100 synthetic events, renders via
  `TestBackend` with no TTY.

### WS3 — Frame + handler test suite
- Implement §4.1 + §4.2 as `src/tui/tests/` (or `#[cfg(test)]` modules).
- AC: `cargo test -p nanosb-cli` covers all frame cases + handler cases, no TTY.

### WS4 — Scripted VM test
- Implement §4.4 as an `#[ignore]`d test.
- AC: passes locally on a VM; documented command.

### WS5 — Local verification harness + docs
- `scripts/tui-smoke.sh` (§4.5) + "Testing the TUI" guide.
- Update `/help` and the README TUI section.
- AC: script runs and prints PASS/FAIL.

## 6. Acceptance criteria

- AC1 Every `/help` entry maps to a working command (test asserts parity).
- AC2 No deprecated stubs remain (grep-clean).
- AC3 Frame tests cover: welcome, 1 panel (loading/error/terminal), multi-panel,
  sidebars, focus, help overlay.
- AC4 Handler tests cover add/close/open/focus/kill/env/theme/zoom/reconnect.
- AC5 Event-loop tests drive 100 synthetic events + render, no TTY.
- AC6 Scripted VM test: add → attach → reconnect → kill, all asserted.
- AC7 `cargo test -p nanosb-cli` green with no TTY; `tui-smoke.sh` PASS.

## 7. Milestones

- **T0** Command audit signed off; WS1 cleanup + generated help.
- **T1** WS2 headless extraction (with regression tests green throughout).
- **T2** WS3 frame + handler suite.
- **T3** WS4 scripted VM verification.
- **T4** WS5 harness + docs; README/help updated.

## 8. Risks

- R1 **`run.rs` is huge** → extraction is the riskiest part; do it incrementally
  behind the existing tests; never a big-bang rewrite.
- R2 **Frame assertions are brittle** → assert key substrings/regions; use
  `insta` snapshots only for stable screens, gated.
- R3 **VM tests need a VM** → keep `#[ignore]` + documented run; CI optional.
- R4 **pty harness is platform-specific** → macOS first; document Linux.
- R5 **Behavior change from removing commands** → acceptable (they were dead);
  note in `/help` and the changelog.

## 9. Open questions

1. Remove the deprecated commands entirely (recommended) or keep one
   `deploy`-style command that shells out to `nanosb apply`?
2. `insta` snapshots vs hand-written buffer assertions?
3. Scripted VM test as a required CI job (macOS runner) or opt-in local only?
4. Should the TUI later surface Epic 1's new `metrics`/`logs`/`fs` (a panels
   epic), or stay focused on agent panels for now?
