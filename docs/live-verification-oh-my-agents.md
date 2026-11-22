# Live verification — oh-my-openagent / oh-my-codex / oh-my-claude

Epic [#238](https://github.com/nanosandboxai/nanosandbox/issues/238) · verification issue
[OMY-8 / #246](https://github.com/nanosandboxai/nanosandbox/issues/246).
Date: 2026-11-20. Host: macOS arm64 (Apple Silicon), Docker 29.7.2, buildx 0.36.1.

Every line below is an observed command result from this working tree.

## 1. Images build and run (linux/arm64)

```bash
docker build -q -t nsb-base:test -f registries/docker/Dockerfile.base registries
docker build -t nsb-omo:test --build-arg BASE_IMAGE=nsb-base:test -f registries/docker/Dockerfile.oh-my-openagent registries
docker build -t nsb-omx:test --build-arg BASE_IMAGE=nsb-base:test -f registries/docker/Dockerfile.oh-my-codex registries
docker build -t nsb-omc:test --build-arg BASE_IMAGE=nsb-base:test -f registries/docker/Dockerfile.oh-my-claude registries
```

| Image | Build | Smoke output |
|---|---|---|
| `oh-my-openagent` | ok (`sha256:fa033a90…`) | `omo 5.1.79 (engine: senpi 2026.11.18-3)` |
| `oh-my-codex` | ok (`sha256:9b9c2fc8…`) | `oh-my-codex v0.22.11` · `codex-cli 0.166.0` · `tmux 3.3a` |
| `oh-my-claude` | ok (`sha256:e3a9e12f…`) | `5.8.4` · `2.1.318 (Claude Code)` |

Each Dockerfile ends with an in-build `--version` assertion, so a broken install fails the build.

## 2. Multi-arch

- **arm64:** built and smoke-tested above.
- **amd64 (emulated):** `docker build --platform linux/amd64` was started for all three.
  The base and QEMU-emulated `npm install -g omo-ai` ran for ~70 minutes without finishing;
  the run was aborted to avoid saturating the host. This is an emulation-speed limit, not
  an install failure — the same install succeeded natively on arm64 and in the Wave 0
  probes on both `node:22-slim` and `node:24-slim`.
- **Authoritative amd64 proof:** `.github/workflows/registry-docker-build.yml` builds
  `linux/amd64,linux/arm64` on native `ubuntu-latest` runners. Dispatch the workflow
  (`agent: ''`, tag `rc…`) after merge and confirm both platform manifests.

## 3. Rust test suites

| Command | Result |
|---|---|
| `cargo test -p sandbox -p gateway` | **278 passed**, 0 failed |
| `cargo test -p nanosb-cli` | **451 passed** + 3 + 2, **0 failed**, 3 ignored |
| `cargo test -p runtime-config` | **26 passed**, 0 failed |
| `cargo test -p runtime --lib` | **66 passed**, 0 failed |

New coverage: serde names (`oh-my-*`, never `ohmycodex`), `FromStr` aliases (`omo`/`omx`/`omc`),
argv table for all 11 agents, state-mount plan, no MCP/skills output, empty-model-list guard,
and the image-detection regression (`oh-my-codex` → `oh-my-codex`, not `codex`).

## 4. Models registry gates

```bash
python3 scripts/models/validate.py            # OK: valid v2 catalog
python3 scripts/models/sync.py --check --offline  # OK: models.json + provider-catalog.json up to date
PYTHONPATH=/tmp/pydeps python3 -m pytest scripts/models -q  # 62 passed
```

Catalog entries: `oh-my-openagent` 4 models / providers `[anthropic, openai]`;
`oh-my-codex` 8 / `[openai]`; `oh-my-claude` 44 / `[anthropic]`.

> The system Python has a shadowed, incomplete `yaml` package, so pytest was run with
> `PyYAML` + `pytest` installed into `/tmp/pydeps` (no global changes). CI installs both.

## 5. Docs gate

```bash
bash scripts/check-docs.sh   # docs check passed
```

`cargo fmt --all --check` exits 0 (no diff).

## 6. CLI surface (real binary)

```bash
cargo build -p nanosb-cli
ANTHROPIC_API_KEY=sk-ant-test OPENAI_API_KEY=sk-test \
  ./target/debug/nanosb apply --dry-run -f /tmp/ohmy-sandbox.yml
```

```
created    omx                      dry-run
created    omc                      dry-run
created    omo                      dry-run
```

The manifest used full names (`type: oh-my-openagent`, `oh-my-claude`) **and** the alias
(`type: omx`), so parsing + bare-image resolution are exercised end to end.

## 7. Known gaps

| Item | Status |
|---|---|
| amd64 local build | Not completed (QEMU too slow). CI buildx is the gate. |
| VM boot / headless E2E | Not run: requires a built libkrun + codesigned `nanosb`. Command: `NANOSB_BINARY_PATH="$PWD/target/release/nanosb" cargo test -p nanosb-cli tui::vm_test -- --ignored --nocapture`, plus `nanosb run --image oh-my-codex --headless -p "echo hi"` against a stub base URL. |
| Images published | Not pushed locally. Workflow publishes on merge / `workflow_dispatch`. |
| `cargo clippy --workspace -- -D warnings` | Fails on **pre-existing** lints in `crates/protocol` (`derivable_impls`, `field_reassign_with_default`) and `runtime-config` (`items_after_test_module` — the tests module sits at HEAD line 968, before the constants at HEAD line 1260). No new lint is attributable to the lines added here. |
| Web docs | Live in `nanosandboxai/nanosandboxai.github.io` (not in this workspace). Pages to change: `content/docs/cli/commands.md`, `content/docs/cli/configuration.md`, `content/docs/cli/tui-mode.md`, `content/docs/architecture/*`. |
