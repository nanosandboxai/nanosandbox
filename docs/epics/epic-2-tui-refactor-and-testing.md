# Epic 2 — TUI refactor, command audit, and testability

Status: draft
Date: 2026-10-07
Related: PR #91 (TUI migrated to supervisor console), Epic 1

## 1. Context (verified 2026-10-07)

**Structure**
- `src/tui/commands.rs` — `Command` enum (**34 variants**) + parser; **81 tests**.
- `src/tui/run.rs` — event loop + command handlers (~5.5k lines); **10 tests**.
- `src/tui/app.rs` — `App`/`AgentPanel` state; **25 tests**.
- `src/tui/renderer.rs` — `pub fn render(frame: &mut Frame, app: &mut App)` — a
  clean, headless entry point (good news for testing).
- `src/tui/{terminal,upload,event,grid,theme,text_input}.rs`.

**The migration left dead/“removed” command paths.** Commands that used to
mutate agent config in-VM now only print a notice pointing at `sandbox.yml` +
`nanosb apply`:
- `/mcp add|remove|enable|disable` → "MCP hot-reload was removed …"
- `/skills add|remove` → "Skill hot-reload was removed …"
- `/agent set` → "Agent hot-reload was removed …"

These are still parsed and listed in `/help`, which is misleading.

## 2. Command support audit (to be confirmed during WS1)

| Command | Status today | Target |
|---|---|---|
| `/quit`, `/destroy`, `/clearhistory` | supported | keep |
| `/help` | supported | regenerate from the enum (single source of truth) |
| `/add` | supported (supervisor spawn) | keep |
| `/close`, `/open`, `/focus`, `/sandboxes`, `/zoom` | supported | keep |
| `/kill` | supported (stops supervisor) | keep |
| `/reconnect` | supported (console) | keep |
| `/copy` | supported | keep |
| `/upload`, `/paste-image` | supported (virtiofs write) | keep |
| `/env` | supported | keep |
| `/edit`, `/branches`, `/gitsync` | supported (host-side git) | keep |
| `/theme` | supported | keep |
| `/mcp` (toggle), `/mcp list` | supported (read-only) | keep |
| `/mcp add\|remove\|enable\|disable` | **deprecated stub** | remove from parser+help, or make an explicit `deploy` hint |
| `/skills list`, `/skills show` | supported (read-only) | keep |
| `/skills add\|remove` | **deprecated stub** | remove or explicit hint |
| `/agent list\|show\|info` | supported (read-only) | keep |
| `/agent set` | **deprecated stub** | remove or explicit hint |

## 3. Goal

Make the TUI **honest, minimal, and testable**:
1. Every advertised command either works or is explicitly removed (no silent stubs).
2. The event loop is separable from the terminal so it can be driven headlessly.
3. A real test suite (frame assertions + scripted-event integration) proves the
   TUI works — runnable locally and in CI.

Non-goals: new TUI features; changing the visual design.

## 4. Workstreams

### WS1 — Command surface cleanup
- Decide per deprecated command: **remove** vs **explicit hint** (recommend:
  remove the mutating MCP/skills/agent variants; keep read-only list/show; make
  the removal discoverable via a single `/deploy` help line).
- Delete dead `Command` variants (`McpAdd/McpRemove/McpEnable/McpDisable`,
  `SkillsAdd/SkillsRemove`, `AgentSet` if removed) and their handlers, or funnel
  them into one `DeployHint` variant.
- **Generate `/help` from the `Command` enum** so text can never drift again.
- AC: `grep` for "was removed" in `src/tui/**` returns nothing (or exactly one
  intentional hint).

### WS2 — Headless event-loop extraction
- Extract the terminal setup from the loop: a function that takes an
  `App` + an event source and runs one step / N steps, independent of
  crossterm + alt-screen.
- Provide an event-injection seam (feed `CrosstermEvent`s) so tests can drive it.
- Ensure `App` is constructible without a real terminal (already close).
- AC: a test can construct `App`, drive 100 synthetic events, and call
  `render()` into a `TestBackend` without a TTY.

### WS3 — TUI test harness
- **Frame/snapshot tests** with `ratatui::backend::TestBackend`: render `App`
  states and assert on the buffer (welcome screen, panel grid, sidebars,
  loading/error states, terminal content).
- **Command-handler tests**: expand beyond parsing — assert side effects on `App`
  state (panel add/close/open/focus, env set, theme switch, kill).
- **Parser tests**: keep the existing 81; add property-ish coverage for the
  deprecated-command removal.
- Consider `insta` snapshots for frames (or hand-written buffer assertions to
  avoid a dependency).
- AC: TUI unit+frame tests run under `cargo test -p nanosb-cli` with **no TTY**
  and no VM.

### WS4 — Local end-to-end verification (scripted, no human)
- A test/example that **boots a real supervised sandbox** (alpine) and drives the
  TUI event loop programmatically:
  1. add a panel → assert the spawn path emits `SupervisorReady`;
  2. attach console → assert streamed bytes reach `AppEvent::TerminalData`;
  3. reconnect → assert re-attach succeeds;
  4. kill → assert the supervisor stops (`nanosb ps` shows it gone).
- Run with `NANOSB_BINARY_PATH` + `--ignored` (like `next_mode_test`), so CI
  stays green without a VM.
- AC: the harness passes locally on a real VM; documented run command.

### WS5 — Docs
- TUI support matrix (from WS1) in `docs/`.
- "Testing the TUI" guide (how to run WS3/WS4).
- Update `/help` and README TUI section.

## 5. Acceptance criteria

- AC1 Every `/help` entry maps to a working command (verified by a test that the
  help listing == the supported set).
- AC2 No deprecated stubs remain (grep-clean).
- AC3 Frame tests cover: welcome, one panel, multi-panel grid, sidebar open,
  loading, error, terminal mode, headless mode.
- AC4 Event-loop tests drive add/close/open/focus/kill/env/theme and assert state.
- AC5 Scripted VM test (WS4) passes locally: add → attach → reconnect → kill.
- AC6 `cargo test -p nanosb-cli` green with no TTY; ignored VM test documented.

## 6. Milestones

- **M0** Command audit signed off (supported vs removed).
- **M1** WS1 cleanup + generated help.
- **M2** WS2 headless extraction.
- **M3** WS3 frame + handler test suite.
- **M4** WS4 scripted VM verification.
- **M5** WS5 docs + README update.

## 7. Risks

- R1 **`run.rs` is large (~5.5k lines)** → extraction is the risky part;
  do it behind tests, one handler group at a time.
- R2 **Frame snapshots are brittle** → assert on key substrings/regions, not
  whole buffers, or gate exact snapshots behind `insta`.
- R3 **VM-dependent tests** → keep them `#[ignore]`d with a documented run.
- R4 **Behavior questions** (e.g., should MCP/skills edits be re-added via a
  redeploy helper?) → decide in M0.

## 8. Open questions

1. Remove the MCP/skills/agent mutating commands entirely, or keep one
   `deploy`-style command that runs `nanosb apply` for you?
2. Add `insta` for frame snapshots, or hand-write buffer assertions?
3. Should the scripted VM verification (WS4) become a required CI job (macOS
   runner with a VM), or stay an opt-in local check?
4. Is `/agent set` meaningfully different from editing `sandbox.yml`, or purely
   redundant now?
