# Plan — Pure agent images + multi-agent support + HOME-based workspace

Status: in progress. Wave 1 running (pure-install Dockerfiles; add-agents Rust). Wave 2 = HOME-based workspace + docs.

## Goal
- Agent images are **pure install only**: install the CLI, nothing else — no user/group definitions, no `/workspace`, no `CMD`, no `configure`.
- `/workspace` is **not mandatory**; the default working dir / project mount becomes the guest user's **HOME**.
- Ship **more agents** (opencode, aider, gemini, crush) in addition to claude/codex/goose/cursor.
- No `configure`/onboarding in any launch path.
- Docs describe exactly how to run each agent, 1:1 with the code, with tests.

## Findings (from investigation)
- Image build chain: `node:22-slim` → `Dockerfile.base` → per-agent `Dockerfile.<agent>`; pushed by
  `.github/workflows/registry-docker-build.yml` (matrix `[claude,goose,codex,cursor]`).
  `Dockerfile.base` creates a `developer` user + `/workspace`; each agent `rm -rf /workspace/*` and sets `CMD`.
- Default image resolution: `normalize_image` in `runtime/crates/runtime-config/src/lib.rs:1232-1261`
  (`DEFAULT_AGENTS_REGISTRY` + `AGENT_IMAGES`). Bare names → `ghcr.io/nanosandboxai/nanosandbox/<n>:latest`.
- Defaults: `default_workdir()` = `/workspace` (runtime-config lib.rs:269); `default_project_mount_point()` = `/workspace`
  (:944); the **TUI re-injects `mount_point: "/workspace"`** at `src/tui/run.rs:4772,5039,5053` (bypasses defaults).
- Guest user/home: agent sandboxes default to `user=developer`, `home=/home/developer` (`src/deploy.rs:235-237`);
  plain `nanosb run` → root / `HOME=/root`. Guest home is materialized + `KRUN_HOME` set in `libkrun.rs:1249-1286`.
- Hard-coded `/workspace` consumers: `src/tui/upload.rs` (`UPLOAD_DIR`, `strip_prefix("/workspace")`),
  `src/tui/run.rs:2987` (`/discard` → `git -C /workspace`), `skills_gen.rs:28` (agent preamble), mounts/tests/docs.

## Workstream A — pure-install images (Wave 1, in progress)
- Rewrite `Dockerfile.base|claude|codex|goose|cursor|legacy` to pure install; add `Dockerfile.{opencode,aider,gemini,crush}`.
- Update the CI matrix. (Owned by the image agent.)

## Workstream B — add agents end-to-end (Wave 1, in progress)
Adding an agent touches these (compiler-forced):
1. `AgentType` enum/`ALL`/`as_str`/`FromStr`/`builtin_agent_defaults` — `sandbox/.../config/mod.rs`.
2. `AGENT_IMAGES` — `runtime-config/src/lib.rs`.
3. `AgentCommandBuilder` (+ `build_*`) — `deploy/agent_cmd.rs`; **interactive = the agent's TUI, headless = its non-interactive flags**.
4. `McpConfigFormat` + `generate_*` — `deploy/config_gen.rs`.
5. `generate_*` — `deploy/skills_gen.rs`.
6. `agent_state_dirs` / `agent_config_guest_paths` / `merged_config_path` — `deploy/mount_planner.rs`.
7. `SUPPORTED_AGENTS` — `src/tui/commands.rs`; `KNOWN_AGENTS`/`detect_agent_type_from_image`/`required_api_keys`/`normalize_agent_name`/session detection — `src/tui/run.rs`.
8. Dockerfile + CI matrix (Workstream A).
9. Tests + docs.
New agents: opencode, aider, gemini, crush.

## Workstream C — HOME instead of mandatory /workspace (Wave 2)
**Design:** default the guest working dir AND the project mount to the guest user's HOME.
**Critical risk — mount nesting:** per-agent state mounts live under `${HOME}/.claude`, `${HOME}/.codex`, etc.
(`mount_planner.rs`). Mounting the project at HOME (parent) shadows those nested mounts. Resolution options:
- (a) project mount = HOME, and MOVE per-agent config **into** the same mount (no separate nested mounts), or
- (b) project mount = a subdir of HOME (e.g. `${HOME}` itself only when no agent config conflicts), or
- (c) mount project at HOME and deliver agent config via the config layer (not a sibling mount).
**Required edits:** `default_workdir()` + `default_project_mount_point()`; remove the three explicit `/workspace`
injections in `src/tui/run.rs`; `src/deploy.rs` user/home default (so HOME is well-defined without an image user);
`upload.rs` (`UPLOAD_DIR`/`strip_prefix`); `/discard` `git -C`; `skills_gen` preamble; tests + docs.
**Coupling:** images no longer define `developer`, so the runtime must create/own the guest user + home at boot
(see `runtime/docs/plans/2026-10-05-zero-image-customization-plan.md`). Image + runtime must land together.

## Workstream D — docs 1:1 + tests (Wave 3)
- `README.md`, `sandbox/README.md` (Supported Agent Types), `agents-registry/README.md`, `config/agent_defaults.yaml`.
- A per-agent "how to run": interactive vs headless, required API key env, image ref.
- Tests: argv parity per agent (already in `agent_cmd.rs`), `/add <agent>` parse, image detection, and (if possible)
  a real sandbox boot per agent. Keep docs generated-checked by `scripts/check-docs.sh`.

## Verification
- `cargo check`/`cargo test -p sandbox -p nanosb-cli -p runtime` after each wave.
- `hadolint`/grep proof of pure Dockerfiles (no user/workspace/CMD/configure).
- Rebuild + codesign + reinstall `nanosb`; `/add <agent>` smoke for each.
