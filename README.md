# Nanosandbox

Monorepo for the Nanosandbox platform — run AI coding agents in hardware-isolated microVM
sandboxes (via [libkrun](https://github.com/containers/libkrun)).

## Repository layout

| Path | Contents |
|------|----------|
| `src/` | `nanosb` CLI + TUI (clap surface in `src/main.rs`, TUI in `src/tui/`) |
| `crates/protocol` | Versioned NDJSON protocol types (source of truth for the wire) |
| `crates/client` | `SandboxClient` + live conformance tests |
| `crates/conformance` | CI-safe protocol conformance harness |
| `crates/host` | Thin host layer: supervisor client + state (no TUI, no FFI) |
| `runtime/` | MicroVM engine (libkrun FFI), OCI images; `runtime/crates/runtime-config` holds the FFI-free config types |
| `sandbox/` | Agent-aware sandbox SDK + `deploy` module + `sandbox.yml` parsing |
| `sdk/` | Eight language SDKs + conformance harness (see [`sdk/README.md`](sdk/README.md)) |
| `agents-registry/` | Agent definitions, skills, MCP sources, model catalog, Docker images |
| `install-deps/` | Runtime dependency (libkrunfw + gvproxy) installer |
| `guest/` | In-guest exec agent (linux/aarch64, cross-compiled) |
| `docs/` | Protocol spec, ADRs, epics/plans, live-verification reports |
| `scripts/`, `test/` | Install/packaging scripts and shell-based CLI/config tests |

## Install

```bash
curl -fsSL https://github.com/nanosandboxai/nanosandbox/releases/latest/download/install.sh | bash
```

This installs the `nanosb` binary, the runtime dependencies (libkrunfw + gvproxy), and the
guest `exec-agent`, then codesigns the binary on macOS.

### Requirements

- **macOS Apple Silicon (arm64)** — stable.
- **Linux x86_64 (KVM)** — live-verified (see [`docs/live-verification-linux.md`](docs/live-verification-linux.md));
  aarch64 uses the same code path but is not yet exercised. Requires `/dev/kvm`.
- **Windows** — support has been archived (see the `archive/windows-track` branch).

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

# Resume the most recent session, or a specific one
nanosb --resume
nanosb --session <session-id>
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

### Language SDKs

Eight protocol clients — **Rust, Python, TypeScript/Node, Go, Ruby, Java, C#/.NET, Kotlin** —
plus a language-agnostic conformance harness, all speaking protocol **v1.0.0**
(35 conformance scenarios). See [`sdk/README.md`](sdk/README.md) and the per-language
READMEs under `sdk/<lang>/`.

```bash
# Run the conformance harness
cargo build -p nanosb-conformance
```

### containerd / CRI

The runtime exposes a shim-facing surface over the same protocol: `exec` with an
optional PTY (tty/cols/rows), a blocking `wait` returning the exit code, and
`kill` with a signal. This is the substrate for containerd/CRI integration on
Linux/KVM. See [`docs/protocol/protocol.md`](docs/protocol/protocol.md) and the
shim contract [`docs/runtime-capability-contract.md`](docs/runtime-capability-contract.md).

The supported supervisor entrypoint is `nanosb supervise`:

```bash
# Own + reap the supervisor as a child of the caller (shim entrypoint): reports
# only once the sandbox is Running and exits with the task exit code.
nanosb supervise --foreground --name my-sandbox --spec spec.json

# Detached (default): prints the child pid and returns immediately.
nanosb supervise --name my-sandbox --spec spec.json
```

A sandbox can boot a **non-OCI rootfs** directly from a host directory (no image
pull, no OCI bundle) — how a containerd snapshotter mount is handed to the VM:

```json
{ "image": "alpine", "name": "s", "rootfs": { "kind": "local_dir", "path": "/var/lib/containerd/snapshots/..." } }
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
nanosb pull <image>            # Pull an image (full registry path required)
nanosb images                  # List cached images
nanosb run <image> [cmd]       # Run a command in a new sandbox
nanosb exec <sandbox> <cmd>    # Execute a command in a running sandbox
nanosb ps [-a]                 # List sandboxes (running, or all with -a)
nanosb logs <sandbox> [-f]     # Tail a sandbox console log
nanosb attach <sandbox>        # Attach to a running sandbox
nanosb stop <sandbox>          # Stop a running sandbox
nanosb restart <sandbox>       # Restart a sandbox
nanosb rm [-f] <sandbox>       # Remove a sandbox
nanosb describe <sandbox>      # Show a sandbox's resolved configuration
nanosb apply [-f sandbox.yml]  # Apply sandbox.yml (redeploy); supports --dry-run/--prune
nanosb doctor                  # Check runtime prerequisites
nanosb cleanup [--dry-run]     # Clean up stale project clones
nanosb sessions [--project P]  # List saved sessions
nanosb projects                # List registered projects
nanosb projects-forget <path>  # Forget a registered project
nanosb gc [--dry-run]          # Reclaim dead supervisor state
nanosb cache prune [--all]     # Reclaim disk space from image cache
nanosb supervise [--foreground] --name <n> --spec <spec.json>  # Spawn a supervisor (host/shim entrypoint)
```

### Global Flags

| Flag | Description |
|---|---|
| `--format text\|json` | Output format (default: text) |
| `-v, --verbose` | Enable debug logging |
| `--config <path>` | Path to sandbox.yml (repeatable) |
| `--sandbox <name>` | Start only the named sandbox |
| `--project <path>` | Project directory to mount |
| `--cpus <n>` | Override CPU cores |
| `--memory <mb>` | Override memory (MB) |
| `--timeout <secs>` | Override timeout (seconds) |
| `--permissions <level>` | Agent permissions: default, accept-edits, allow-all |
| `-e KEY=VALUE` | Inject environment variable |
| `--env-file <path>` | Load env vars from file |
| `-r, --resume` | Resume the most recent saved session |
| `--session <id>` | Resume a specific session from history |

### Environment Variables

| Variable | Description |
|---|---|
| `NANOSANDBOX_HOME` | Base install directory (default: `~/.nanosandbox`) |
| `INSTALL_DIR` | CLI binary directory (default: `$NANOSANDBOX_HOME/bin`) |
| `NANOSB_VERSION` | Version to install (default: latest) |
| `DEPS_VERSION` | Runtime-deps version to install (default: latest) |
| `NANOSB_BINARY` | Path to a local `nanosb` binary to install (skips download) |
| `NANOSB_EXEC_AGENT` | Path to a local `exec-agent` binary to install (skips download) |
| `NANOSB_HOME` | Runtime state root (default: `~/.nanosandbox`) |
| `NANOSB_LOG` | Log level for the file log |
| `NANOSB_REGISTRY_PATH` | Path to the agents registry (default: `~/.nanosandbox/agents-registry`) |
| `NANOSB_GUEST_BIN_DIR` | Directory holding `exec-agent` (default: `$NANOSB_HOME/guest-bin`) |
| `NANOSB_BINARY_PATH` | Path to the `nanosb` binary used to spawn the supervisor |

## sandbox.yml

Sandboxes are configured via `sandbox.yml`:

```yaml
defaults:
  cpus: 2
  memory: 4096
  timeout: 600
  permissions: default
  workspace:
    mode: isolated        # isolated (default) | shared
    fresh_rootfs: false

sandboxes:
  claude:
    image: claude
    env:
      ANTHROPIC_API_KEY: ${ANTHROPIC_API_KEY}
    env_file: .env.claude
    agent: python-developer
    skills: [git-workflow, tdd]
    model: claude-sonnet-4
    permissions: accept-edits
    mcp:
      github:
        command: npx
        args: ["-y", "@modelcontextprotocol/server-github"]
    network:
      enabled: true
      mode: tsi             # tsi (default) | bridge
      scope: public         # public (default) | private
      ports: ["8080:80"]
      dns: ["1.1.1.1"]
    docker:
      enabled: false      # Docker starts a dockerd in the VM when enabled
      network: host       # host (default) | none | bridge (bridge requires the docker_bridge capability)
    mounts:
      - host: ./data
        container: /data
        readonly: true
    project:
      path: .
      branch: main
      mount_point: /workspace
      auto_sync: false

  codex:
    image: codex
    cpus: 4
    run_as_root: false
    env:
      OPENAI_API_KEY: ${OPENAI_API_KEY}
    auto_mode: true
    prompt: "Refactor the module in /workspace."

  cursor:
    type: cursor
    image: cursor
    interactive: true
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

Housekeeping: `/disk` shows state usage, `/gc` reclaims dead supervisor dirs
(detected by live control-socket/PID liveness), and `nanosb gc` /
`nanosb cleanup --dry-run` reclaim disk without touching session-referenced
clones.

Other TUI commands: `/logs [n]` (console tail), `/stop [n|name]` (stop a
sandbox, keep the panel), `/exec <cmd>` (run in an exec-channel sandbox),
`/mounts`, and `/apply` (fast-forward-merge `refs/nanosb/<id>` into your
branch). Registered projects can be listed and switched with `/projects` and
`/project <n|path>` — panels from different projects coexist in one TUI.

## Advanced sandbox surface

Beyond a single container, a sandbox can expose persistent volumes, disk
snapshots, published ports, and an explicit security context. Design decisions:

- [Snapshots](docs/decisions/snapshots.md) — disk-only snapshots.
- [Volume backings](docs/decisions/volume-backings.md) — directory-backed volumes.
- [Networking & dynamic publish scope](docs/decisions/networking-dynamic-publish-scope.md).
- [Security context contract](docs/decisions/security-context-contract.md).
- [Multi-container-per-VM](docs/decisions/multi-container-per-vm.md) — v1 keeps one container per VM.
- [Docker in microVM](docs/decisions/docker-in-microvm.md) — threat model + config surface (daemon starts when enabled; use `docker run --network host` for outbound; Tier 2 bridge networking ships when the helper advertises the `docker_bridge` capability).

## Packaging

Self-contained per-language runtime bundles (helper + libkrunfw + gvproxy +
`exec-agent` + SDK sources + `LICENSE`) can be produced with
[`scripts/package-sdk.sh`](scripts/package-sdk.sh); see
[`docs/runtime-setup.md`](docs/runtime-setup.md) for the runtime setup and bundle layout.

## License

Apache-2.0
