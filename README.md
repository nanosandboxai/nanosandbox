# Nanosandbox

Monorepo for the Nanosandbox platform — run AI coding agents in isolated microVM sandboxes.

## Repository layout

| Path | Contents |
|------|----------|
| `.` (`src/`, `config/`, `scripts/`, `test/`) | `nanosb` CLI + TUI |
| `runtime/` | MicroVM engine (libkrun), libkrunfw/gvproxy dependencies |
| `sandbox/` | Agent-aware sandbox SDK + host-side deploy module |
| `agents-registry/` | Agent definitions, skills, MCP sources, Docker images |
| `install-deps/` | Runtime dependency installer scripts |

## Install

```bash
curl -fsSL https://github.com/nanosandboxai/nanosandbox/releases/latest/download/install.sh | bash
```

This installs the `nanosb` binary along with runtime dependencies (libkrun, gvproxy) and codesigns the binary on macOS.

### Requirements

- macOS Apple Silicon (arm64)
- Linux is not yet supported
- Windows support has been archived (see `archive/windows-track` branch)

### Build from Source

```bash
git clone https://github.com/nanosandboxai/nanosandbox.git
cd nanosandbox
cargo build --release -p nanosb-cli
```

The binary will be at `target/release/nanosb`.

## Quick Start

```bash
# Check runtime prerequisites
nanosb doctor

# Pull an agent image
nanosb pull ghcr.io/nanosandboxai/agents-registry/claude:latest

# Run a sandbox from sandbox.yml in the current directory
nanosb
```

## Usage

### TUI Mode

Running `nanosb` with no subcommand launches the interactive TUI. It auto-detects `sandbox.yml` in the current directory and starts sandboxes with a terminal multiplexer interface.

```bash
# Launch TUI with auto-detected config
nanosb

# Launch with explicit config file
nanosb --config path/to/sandbox.yml

# Launch a specific sandbox from config
nanosb --sandbox claude

# Mount a project directory into sandboxes
nanosb --project /path/to/project

# Override resources
nanosb --cpus 4 --memory 8192 --timeout 1200

# Provide runtime env pool for this TUI run
nanosb --env OPENAI_API_KEY=... --env-file .env.local
```

Notes:
- In TUI mode, startup env is runtime-only (not persisted to sessions).
- `sandbox.yml` env values take precedence over startup env for matching keys.
- Local `.env` is auto-loaded only when no `sandbox.yml` is active.
- `/add` does not import startup env automatically; select keys explicitly with `--use-env`.
- Type `/help` in the TUI for the full command list. The command surface is
  generated from a single table, so it is always in sync with what the TUI
  accepts.
- Agent, MCP, and skill configuration is **declarative**: it lives in
  `sandbox.yml` and is applied by redeploying (`nanosb apply`). There is no
  in-TUI hot-reload.

```bash
# Add panel and import selected startup env keys
/add claude --use-env OPENAI_API_KEY --use-env GITHUB_TOKEN
```

### Testing the TUI

The TUI has a headless test suite (no terminal, no VM) plus an opt-in
end-to-end check. See [runtime/docs/TESTING.md](runtime/docs/TESTING.md) for the full guide.

```bash
# Frame, handler, and event-loop tests (no TTY, no VM) — runs in CI
cargo test -p nanosb-cli

# Scripted VM end-to-end test (requires libkrun/gvproxy + a codesigned
# binary; opt-in). Run with the real nanosb binary via NANOSB_BINARY_PATH.
NANOSB_BINARY_PATH="$PWD/target/debug/nanosb" \
  cargo test -p nanosb-cli tui::vm_test -- --ignored --nocapture

# Local pty smoke harness: drives the TUI under a real PTY
scripts/tui-smoke.sh
```

### CLI Commands

```bash
nanosb pull <image>          # Pull an image (full registry path required)
nanosb images                # List cached images
nanosb run <image> [cmd]     # Run a command in a new sandbox
nanosb exec <sandbox> <cmd>  # Execute a command in a running sandbox
nanosb ps [-a]               # List sandboxes (running, or all with -a)
nanosb stop <sandbox>        # Stop a running sandbox
nanosb rm [-f] <sandbox>     # Remove a sandbox
nanosb doctor                # Check runtime prerequisites
nanosb cleanup               # Clean up stale project clones
nanosb cache prune [--all]   # Reclaim disk space from image cache
```

### Global Flags

| Flag | Description |
|---|---|
| `--format text\|json` | Output format (default: text) |
| `--verbose` | Enable debug logging |
| `--config <path>` | Path to sandbox.yml (repeatable) |
| `--sandbox <name>` | Start only the named sandbox |
| `--project <path>` | Project directory to mount |
| `--cpus <n>` | Override CPU cores |
| `--memory <mb>` | Override memory (MB) |
| `--timeout <secs>` | Override timeout (seconds) |
| `--permissions <level>` | Agent permissions: default, accept-edits, allow-all |
| `-e KEY=VALUE` | Inject environment variable |
| `--env-file <path>` | Load env vars from file |

### Environment Variables

| Variable | Description |
|---|---|
| `NANOSB_VERSION` | Version to install (default: latest) |
| `INSTALL_DIR` | Binary install directory (default: `~/.local/bin`) |

## sandbox.yml

Sandboxes are configured via `sandbox.yml`:

```yaml
defaults:
  cpus: 2
  memory: 4096
  timeout: 600

sandboxes:
  claude:
    image: claude
    env:
      ANTHROPIC_API_KEY: ${ANTHROPIC_API_KEY}
    mcp:
      github:
        command: npx
        args: ["-y", "@modelcontextprotocol/server-github"]

  codex:
    image: codex
    cpus: 4
    env:
      OPENAI_API_KEY: ${OPENAI_API_KEY}
```

Bare image names (e.g., `claude`, `codex`) are automatically resolved to `ghcr.io/nanosandboxai/agents-registry/<name>:latest`.

### Interactive agent panels

By default an agent panel runs **headless**: the agent's output is captured as
structured events. Set `interactive: true` to instead run the agent's own
interactive UI in the panel, on a real TTY (keystrokes are forwarded to it):

```yaml
sandboxes:
  claude:
    image: localhost:5050/claude:latest
    interactive: true
```

Or per panel from the TUI:

```
/add claude --image localhost:5050/claude:latest --interactive
```

`interactive` and `auto_mode` are mutually exclusive. Interactive panels are
backed by the in-guest exec agent's PTY (the same channel as `nanosb exec --tty`).

### Reviewing and applying agent changes

Agent changes stay in the sandbox clone until you review them. From the TUI:

```
/diff [--stat]     # show what the agent changed vs the base commit
/status            # branch, dirty files, sync state
/sync [--dry-run]  # fetch agent commits to refs/nanosb/<id> in the source repo
/discard           # reset the clone to its base commit
```

The source repo is **read-only to nanosb**: no branches are created in it and
changes are never written to `refs/heads/*` — you review and apply them yourself
(`git fetch origin refs/nanosb/<id>` or the equivalent). Non-git project
directories are never modified (the repo is initialised inside the clone).

Housekeeping: `/disk` shows state usage, `/gc` reclaims dead supervisor dirs,
and `nanosb gc` / `nanosb cleanup --dry-run` reclaim disk without touching
session-referenced clones.

## License

Apache-2.0
