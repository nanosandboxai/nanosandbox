# GitSync Settings & External Tool Integration

## Problem

Auto-sync (`sync_project_commits()`) automatically fetches agent commits from the sandbox clone to the user's local repo branch. This is convenient but dangerous:

1. **Overwrite risk** — force-fetch can overwrite the user's own work on the same branch
2. **Unreviewed code** — agent commits land in the local repo without review

Additionally, users want to inspect agent work before syncing — using tools they already have (VS Code, Cursor, gitui, lazygit, etc.).

## Design

### 1. User Settings (`~/.nanosandbox/config.toml`)

New persistent config file with TOML format:

```toml
[gitsync]
# WARNING: auto_sync modifies your local repo branches automatically.
# Agent commits are fetched to your source repo as they happen.
# Default: false (safe — commits stay in the isolated clone until you sync manually).
auto_sync = false
notify_on_commit = true   # Show system message when agent commits (regardless of auto_sync)

[tools]
# Preferred tool for /open command. "auto" detects first available.
# Options: "auto", "gitui", "lazygit", "tig", "vscode", "cursor", "gitkraken", "fork", "custom"
editor = "auto"
# custom_command = "my-tool {path}"   # Used when editor = "custom"
```

**Implementation:** New `src/settings.rs` module with `UserSettings` struct. Serde derive for TOML. `load()` returns defaults if file doesn't exist. `save()` writes back.

### 2. Lazy Branch Creation

When gitsync is **off** (default), `ProjectMount::setup()` does NOT create a `nanosb/<id>` branch in the source repo. The clone is created from the current HEAD. The agent works in complete isolation — the source repo is untouched.

When the user runs `/gitsync now` or `/gitsync on`, THEN the branch is created in the source repo and commits are fetched.

This means:
- `ProjectMount::setup()` receives a `create_source_branch: bool` parameter (derived from settings)
- When `false`: clone happens, agent works, but `created_branches` stays empty
- `/gitsync now` creates the branch in source and populates `created_branches`, then fetches
- `/gitsync on` does the same + enables auto-sync going forward

### 3. Modified `sync_project_commits()`

Current behavior: poll HEAD → detect change → fetch to source → notify.

New behavior:
- **auto_sync off (default):** poll HEAD → detect change → notify only ("New commit `<sha>`: `<subject>`"). No fetch.
- **auto_sync on:** poll HEAD → detect change → fetch to source → notify ("Synced `<sha>` to `<branch>`: `<subject>`").
- Per-panel `sync_override: Option<bool>` takes priority over global setting.

### 4. New TUI Commands

**`/gitsync`** — Git sync management:
- `/gitsync` — show status: auto on/off, last commit SHA, branch name
- `/gitsync on` — enable auto-sync for this panel. Creates source branch if not yet created. Shows warning: "Auto-sync enabled. Agent commits will be fetched to your local branch automatically. This can be unsafe — use /gitsync off to disable."
- `/gitsync off` — disable auto-sync for this panel
- `/gitsync now` — one-shot sync: create source branch if needed, fetch all commits, show summary

**`/open`** — Open clone directory in external tool:
- `/open` — open in preferred tool (from config or auto-detected)
- `/open <tool>` — open in specific tool (override for this invocation)
- Supported tools: `gitui`, `lazygit`, `tig`, `vscode`, `cursor`, `gitkraken`, `fork`

### 5. Tool Launch Behavior

**TUI tools** (gitui, lazygit, tig) — suspend-and-launch:
1. Leave alternate screen (`LeaveAlternateScreen`)
2. Disable raw mode
3. Spawn tool as child process, wait for exit
4. Re-enable raw mode, enter alternate screen
5. Force full redraw

**GUI tools** (VS Code, Cursor, GitKraken, Fork) — fire-and-forget:
1. Spawn `code <path>` / `cursor <path>` / etc. with suppressed stdout/stderr
2. Show system message: "Opened in VS Code"

**Auto-detection order:** gitui → lazygit → tig → code → cursor

Detection via `which <binary>` check.

### 6. Sidebar Sync Indicator

In the sandbox list, show sync status per panel:
- No project mount: no indicator
- Gitsync off: show clone-only icon (e.g., `[clone]`)
- Gitsync on: show sync icon (e.g., `[sync]`)

## Files to Change

| File | Change |
|------|--------|
| **New `src/settings.rs`** | `UserSettings` struct, load/save TOML, tool detection |
| `src/lib.rs` | Export `settings` module |
| `src/tui/app.rs` | Store `UserSettings` on `App`, add `sync_override` to `AgentPanel`, modify `sync_project_commits()` |
| `src/tui/commands.rs` | Add `GitSync` and `Open` command variants, autocomplete entries |
| `src/tui/run.rs` | Handle `/gitsync` and `/open` commands, suspend-and-launch for TUI tools |
| `src/tui/renderer.rs` | Sync status indicator in sandbox list |
| `src/project.rs` | `setup()` accepts `create_source_branch` param, add `create_branch_and_fetch()` method for lazy creation |

No changes to `config.rs`, Docker images, or agent-gateway.

## Verification

1. `cargo build --features cli` — compiles
2. `cargo test --features cli` — all tests pass
3. Manual: launch sandbox with project, verify no branch in source repo (gitsync off by default)
4. Manual: `/gitsync now` creates branch and syncs
5. Manual: `/gitsync on` enables auto-sync with warning
6. Manual: `/open gitui` suspends TUI, launches gitui, resumes on exit
7. Manual: `/open vscode` opens VS Code (fire-and-forget)
8. Manual: config file created/loaded correctly from `~/.nanosandbox/config.toml`
