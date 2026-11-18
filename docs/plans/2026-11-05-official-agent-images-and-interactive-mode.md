# Plan — Official agent images + interactive-mode correctness

Status: **Workstream B implemented**; Workstream A proposed (needs a decision on A2).

## Findings

### Default image resolution (single funnel)
- `runtime/crates/runtime-config/src/lib.rs:1232-1261` — `DEFAULT_AGENTS_REGISTRY`
  (`ghcr.io/nanosandboxai/nanosandbox`) + `AGENT_IMAGES`
  (`base, claude, codex, cursor, goose, legacy`) + `normalize_image`.
  Bare names → `ghcr.io/nanosandboxai/nanosandbox/<name>:latest`; anything else
  → `docker.io/library/<name>:latest`.
- Re-exported via `sandbox/crates/sandbox/src/config/mod.rs:420`; called by the TUI
  (`src/tui/run.rs:4551,4973`), `sandbox.yml` (`config/file.rs:275`), CLI `run`
  (`src/main.rs:1484`) and the host spec (`crates/host/src/spec.rs:75`).
- `SUPPORTED_AGENTS = [claude, goose, codex, cursor]` (`src/tui/commands.rs:172`).
- Images are self-built: `agents-registry/docker/Dockerfile.{base,claude,codex,goose,cursor}`
  (`base` = `node:22-slim`), pushed by `.github/workflows/registry-docker-build.yml`.

### Official upstream image availability
| Agent | Official image | Notes |
|---|---|---|
| **Goose** | ✅ `ghcr.io/block/goose:latest` | ready-to-run, non-root, Apache-2.0 |
| **Claude Code** | ❌ none | devcontainer *feature* only (`ghcr.io/anthropics/devcontainer-features/claude-code:1.0`); install = npm `@anthropic-ai/claude-code` or `claude.ai/install.sh` |
| **Codex CLI** | ❌ none | `ghcr.io/openai/codex-universal:latest` is a base *environment* (no `codex` binary); install = `chatgpt.com/codex/install.sh` / npm `@openai/codex` |
| **Cursor** | ❌ none | install script `cursor.com/install` only |

### Interactive-mode argv (before fix)
- `build_codex` (`agent_cmd.rs:147`) **always** prepended `exec` → interactive ran
  `codex exec` (the non-interactive subcommand) with no prompt → "does not interact".
- `build_goose` interactive emitted `configure` (setup wizard), not the agent session.
- Upstream Codex removed `--full-auto` (2026); current flags are
  `--dangerously-bypass-approvals-and-sandbox` (`--yolo`) / `--approve-for-me`.

## Workstream A — default images (proposed)

**Goal:** stop defaulting to self-built images; use official images where possible.

- **A1 (do now, low risk):** replace the single registry prefix with a per-agent
  default-image map; set `goose` → `ghcr.io/block/goose:latest`. Single edit point:
  `normalize_image` in `runtime/crates/runtime-config/src/lib.rs`.
- **A2 (needs a decision):** Claude/Codex/Cursor have no official CLI image. Options:
  1. **Thin wrappers over official bases** — e.g. `FROM node:22-slim` +
     `npm i -g @anthropic-ai/claude-code`; codex over `ghcr.io/openai/codex-universal`;
     cursor over `debian:bookworm-slim` + `cursor.com/install`. Still a build, but over
     upstream bases.
  2. **Runtime install** — default to a vanilla official base and install the CLI at
     boot via the "next mode" init hooks (aligns with
     `runtime/docs/plans/2026-10-05-zero-image-customization-plan.md`). No image build
     at all, at the cost of first-boot latency/network.
- **A3:** retire (or gate behind a flag) `registry-docker-build.yml` and the per-agent
  Dockerfiles once A1/A2 land; decide the fate of `base`/`legacy` (no upstream peer).
- **A4 (docs, per the repo's docs mandate):** update `README.md` (57, 285, 296, 303),
  `agents-registry/README.md` (102-105, 119-122), `docs/release-verification.md` (81-82),
  `sandbox/README.md:32`.

## Workstream B — interactive mode (DONE)

| Agent | Interactive argv | Change |
|---|---|---|
| **Codex** | `codex` (bare) | was `codex exec` — **fixed** (`agent_cmd.rs::build_codex`) |
| **Goose** | `goose session` | was `goose configure` — **fixed** (`agent_cmd.rs::build_goose`) |
| Claude | `claude` (bare) | already correct |
| Cursor | `cursor-agent` (bare) | already correct |

Tests updated/added in `agent_cmd.rs`:
`test_codex_interactive_is_bare_codex`, `test_goose_interactive_uses_session`
(plus existing headless tests still green). Full `sandbox` suite: 212 passed.

## Verification
- `cargo test -p sandbox` → 212 passed.
- Rebuilt + codesigned + reinstalled `nanosb` 0.3.0.
- Manual (needs an agent API key): `/add codex --interactive` should now enter Codex's
  TUI; `/add goose --interactive` should start a goose session.
