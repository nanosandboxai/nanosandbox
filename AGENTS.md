# AGENTS.md

Guidance for AI coding agents (and humans) working in the **Nanosandbox** monorepo.

Nanosandbox runs AI coding agents inside hardware-isolated microVM sandboxes
(via [libkrun](https://github.com/containers/libkrun)). This monorepo contains
the `nanosb` CLI + TUI, the microVM runtime, the agent-aware sandbox SDK, the
protocol-first language SDKs, the agents registry, and the release/dependency
tooling.

## Repository layout

| Path | Contents |
|------|----------|
| `src/` | `nanosb` CLI + TUI (clap surface in `src/main.rs`, TUI in `src/tui/`) |
| `crates/protocol` | Versioned NDJSON protocol types (source of truth for the wire) |
| `crates/client` | `SandboxClient` + live conformance tests |
| `crates/conformance` | CI-safe protocol conformance harness |
| `crates/host` | Thin host layer: supervisor client + state (no TUI, no FFI) |
| `runtime/` | MicroVM engine (libkrun FFI), OCI images; `runtime/crates/runtime-config` is the FFI-free config-types crate (re-exported as `runtime::config`) |
| `sandbox/` | Agent-aware sandbox SDK + `deploy` module + `sandbox.yml` parsing |
| `sdk/` | 8 language SDKs (Rust, Python, TypeScript/Node, Go, Ruby, Java, C#/.NET, Kotlin) + `conformance/` |
| `agents-registry/` | Agent definitions, skills, MCP sources, model catalog, Docker images |
| `install-deps/` | Runtime dependency (libkrunfw + gvproxy) installer |
| `guest/crates/exec-agent` | In-guest exec agent (linux/aarch64, cross-compiled) |
| `docs/` | Protocol spec, ADRs, epics/plans, live-verification reports |
| `scripts/` | Install, packaging, and smoke-test scripts |
| `test/` | Shell-based CLI/config integration tests |

> Content under `runtime/deps/**` is **vendored/submodule** (libkrunfw, gvproxy).
> Do not edit or document files there.

## Build, test, and lint

```bash
# Build the CLI (debug): binary at target/debug/nanosb
cargo build -p nanosb-cli

# Build the CLI (release)
cargo build --release -p nanosb-cli

# TUI + CLI tests (no TTY, no VM) — runs in CI
#   nanosb-cli includes the headless TUI suite (src/tui/tests.rs, ratatui TestBackend)
cargo test -p nanosb-cli
cargo test -p sandbox -p gateway
cargo test -p runtime --lib

# Workspace checks (what CI runs)
cargo check -p nanosb-cli -p sandbox -p gateway -p runtime

# Format and lint
cargo fmt --all
cargo clippy --workspace -- -D warnings
```

Opt-in / environment-dependent checks:

```bash
# Build the libkrun static library (first time, requires submodules)
runtime/scripts/build-libkrun.sh

# Scripted VM end-to-end test (requires libkrun/gvproxy + a codesigned binary)
NANOSB_BINARY_PATH="$PWD/target/debug/nanosb" \
  cargo test -p nanosb-cli tui::vm_test -- --ignored --nocapture

# Protocol conformance harness
cargo build -p nanosb-conformance

# Local PTY smoke harness (launch + /help only — NOT the TUI test suite)
scripts/tui-smoke.sh
```

## TUI is tested in Rust (MANDATORY)

The TUI is Rust, so its behavior MUST be covered by the Rust test harness under
`src/tui/` — not only by the shell scripts. `scripts/tui-smoke.sh` is a PTY
launch smoke check, never the primary TUI coverage.

- **Frames, layout, styling** — render with `ratatui::backend::TestBackend` and
  assert on the buffer text and per-cell styles. Helpers live in
  [`src/tui/tests.rs`](src/tui/tests.rs) (`render_screen` / `render_backend`).
  No TTY, no VM.
- **Commands, keybindings, events** — drive `handle_command` / `handle_event`
  from `#[tokio::test]`, plus the synthetic event loop, in the same file.
- **End-to-end lifecycle** (create → connect → send → reconnect → kill over the
  real supervisor + in-guest exec channel) — [`src/tui/vm_test.rs`](src/tui/vm_test.rs).
  `NANOSB_BINARY_PATH` MUST point at a codesigned `nanosb` binary (the supervisor
  is spawned from it; an unset/wrong path makes the panel never connect):
  ```bash
  NANOSB_BINARY_PATH="$PWD/target/release/nanosb" \
    cargo test -p nanosb-cli tui::vm_test -- --ignored --nocapture
  ```

Every TUI change — a new slash command, keybinding, layout/grid, panel state, or
theme — MUST add or update Rust tests under `src/tui/tests.rs` (or
`vm_test.rs`) in the same change, and they MUST pass. Shell harnesses are
additive only.

## Conventions

- **Rust edition 2021.** Match the existing style in the file you are editing.
- **Single version source of truth.** The canonical version lives in the root
  `VERSION` file. `scripts/set-version.sh <semver>` writes it there and rewrites
  every component manifest that carries a version (root `Cargo.toml`
  `[workspace.package] version`, the workspace-member entries in `Cargo.lock`,
  `sdk/python/pyproject.toml`, `sdk/typescript/package.json`, `sdk/java/pom.xml`,
  and `guest/crates/exec-agent/Cargo.toml`). Member crates use
  `version.workspace = true`. Exception: `guest/crates/exec-agent` is excluded
  from the workspace (linux-only, cross-compiled) and carries its own version.
- **Never suppress type errors** (`as any`, `@ts-ignore`, `unwrap_or_default`
  used to hide a real error, etc.). Fix the cause.
- **Keep changes minimal and focused.** Do not refactor unrelated code in the
  same change. Fix bugs minimally; never refactor while fixing.
- **Atomic commits.** One logical change per commit, with a conventional-commit
  subject (`feat:`, `fix:`, `chore:`, `docs:`, `ci:`) matching existing history.
- Never commit unless the user asked for it.

## Versioning and releases (MANDATORY)

The canonical version is the root `VERSION` file; `scripts/set-version.sh <semver>`
propagates it to every manifest (see "Single version source of truth" above).
The release pipeline (`.github/workflows/release.yml`) is **tag-driven**: pushing
a `v*` tag runs `scripts/set-version.sh` with the tag's version and publishes all
artifacts under that version. Never hand-edit a single manifest.

### Choose the bump from the largest change in the release

We follow SemVer, applied pre-1.0 (`0.y.z`). Pick the highest level that **any
single change** in the release warrants. It is decided by the *furthest-reaching*
change, not by the number of changes and not by an average.

| Bump | When any single change qualifies | Examples |
|------|----------------------------------|----------|
| **MAJOR** `X.0.0` | Breaking change to a **stable public contract** | `SandboxSpec` / wire-protocol break; removing or renaming a CLI command/flag; removing a `sandbox.yml` field or changing its type incompatibly; an install-layout change that breaks existing installs; dropping a supported platform |
| **MINOR** `0.Y.0` | **Backward-compatible** new surface, or a notable behavior / milestone change | new CLI command/flag; new `sandbox.yml` field; new platform, SDK, or agent; a security-model or release-pipeline overhaul |
| **PATCH** `0.0.Z` | **Backward-compatible** fix with no new surface | bug fix; documentation-accuracy correction; dependency bump; internal refactor |

Rules:

- **Furthest-reaching wins.** One breaking change in an otherwise fix-only release
  is still a MAJOR-class release.
- **Pre-1.0 exception.** While the version is `0.y.z`, breaking changes take a
  **MINOR** bump. Reserve `v1.0.0` for when you deliberately declare the public
  surface stable. Call out every breaking change in the release notes.
- **1.0 gate.** Promote `0.x` → `1.0.0` only once the wire `SandboxSpec`, the
  `nanosb` CLI surface, and the `sandbox.yml` schema are frozen and
  version-compatible across all three front doors (local, cloud node agent,
  containerd shim).
- **Docs move with the version.** Update every doc that states the current
  version (install examples, component READMEs) in the same change. Leave purely
  historical references (e.g. "in v0.2.0 we changed X") intact.

### Cutting a release

1. `scripts/set-version.sh <X.Y.Z>` — writes `VERSION` and every manifest; then
   `scripts/test-set-version.sh` must pass.
2. Commit `VERSION` + all manifests together (`chore(release): vX.Y.Z`).
3. `git tag vX.Y.Z && git push origin vX.Y.Z` — the tag triggers
   `release.yml`, which builds and publishes the release (assets, checksums,
   install scripts).
4. Verify the published assets and write release notes that summarize the changes
   and call out any breaking change.

## Documentation is part of the change (MANDATORY)

**Any change or refactor MUST update the documentation it affects, in the same
change.** Documentation is not a follow-up task.

Before you consider a change complete, search for and update every affected
first-party document:

1. **Root** [`README.md`](README.md) — repository layout, install, CLI surface,
   `sandbox.yml`, platform support.
2. **Component READMEs** — [`runtime/README.md`](runtime/README.md),
   [`sandbox/README.md`](sandbox/README.md),
   [`agents-registry/README.md`](agents-registry/README.md),
   [`install-deps/README.md`](install-deps/README.md).
3. **SDK docs** — [`sdk/README.md`](sdk/README.md) and `sdk/<lang>/README.md`.
4. **Docs** — everything under `docs/**/*.md` (protocol spec, ADRs,
   live-verification reports).
5. **This file** — `AGENTS.md`, if conventions, layout, or commands change.

The rule, concretely:

- If you add/remove/rename a **CLI command or flag**, update the README CLI/flag
  lists — the surface is defined in `src/main.rs`.
- If you add/remove a **`sandbox.yml` field**, update the README config example —
  the schema is defined in `sandbox/crates/sandbox/src/config/file.rs`.
- If you change a **config field, env var, file path, default, or platform
  support statement**, update every doc that mentions it.
- Remove or correct any doc that describes behavior the code no longer has
  (stale docs are treated as bugs).
- Mark superseded design/plan docs with a clear **superseded** note rather than
  leaving them misleading.

A quick self-audit before finishing:

```bash
git diff --name-only            # what changed
grep -rn "<the symbol/command/field you changed>" --include='*.md' . \
  | grep -v runtime/deps
```

Vendored content under `runtime/deps/**` is out of scope.

### Web documentation sync (MANDATORY)

The public documentation site (`https://nanosandboxai.github.io`) is a separate
repository: `nanosandboxai/nanosandboxai.github.io`, with its Markdown sources
under `content/docs/**/*.md`. **Every change that alters the CLI surface,
`sandbox.yml` schema, environment variables, defaults, or user-visible behavior
MUST also update the corresponding page(s) there** — repo docs and web docs move
together, never as a follow-up.

Minimum web-doc touch-points for a new/renamed surface:

- New/changed **CLI command or flag** → `content/docs/cli/commands.md` and, for
  global flags, `content/docs/cli/global-flags.md`.
- New/changed **`sandbox.yml` field** → `content/docs/cli/configuration.md`.
- New/changed **env var or admission/resource behavior** →
  `content/docs/cli/tui-mode.md` and `content/docs/architecture/resource-management.md`.
- New **concept or subsystem** → a page under `content/docs/architecture/`,
  registered in that folder's `_meta.json`.

Do not merge a feature whose web docs are missing. When a feature spans both
repositories, open the web-docs change in the same PR series and link it from the
primary issue/PR.

## libkrun fork — check for new changes on every runtime change (MANDATORY)

The runtime boots via a **fork** of libkrun — `nanosandboxai/libkrun`, branch
`nanosandbox` — whose revision is pinned by `LIBKRUN_SHA` in
[`runtime/scripts/build-libkrun.sh`](runtime/scripts/build-libkrun.sh) and built
into `libkrun.a` (linked by `runtime/crates/libkrun-sys`). The fork carries
nanosandbox commits on top of a pinned upstream tag: the guest-init hooks
(`KRUN_NEXT_MODE`, `KRUN_EXTRA_MOUNTS`, `KRUN_NET_CONFIG`, `KRUN_EXEC_AGENT`) and
macOS virtiofs hardening. The fork's `main` tracks upstream and has diverged from
`nanosandbox` — never build from or push customizations to `main`.

Before you touch the VM boot path, the guest init, vsock/console/networking FFI,
or any pinned-libkrun feature, you MUST:

1. **Check the fork for newer work** — compare the pinned SHA against the fork's
   `nanosandbox` branch and upstream:
   ```bash
   gh api "repos/nanosandboxai/libkrun/commits?sha=nanosandbox&per_page=20" \
     --jq '.[] | "\(.sha[0:8]) \(.commit.message | split("\n")[0])"'
   ```
   to see whether the feature you need already exists, or a change you depend on
   moved. Validate the new feature against our changes before adopting it.
2. **Keep the init-hook contract in sync.** Our runtime sets the init env vars in
   `runtime/crates/runtime/src/runtime/libkrun.rs`; the fork's init reads them in
   `src/init_blob/init/init.c` on the `nanosandbox` branch. A rename or format
   change on either side must move both sides together.
3. **Bump deliberately.** When you bump `LIBKRUN_SHA`, commit on the fork's
   `nanosandbox` branch, rebuild with `runtime/scripts/build-libkrun.sh`, record
   the new commit in
   [`runtime/scripts/patches/README.md`](runtime/scripts/patches/README.md), and
   re-verify boot/exec on the Linux/KVM host.

## Runtime compatibility: local + cloud MUST always stay compatible (MANDATORY)

The `nanosb` runtime is one engine behind three front doors, and they must
**always** stay mutually compatible and version-compatible:

- **Local (personal machine):** the `nanosb` CLI (`nanosb run`, `nanosb
  supervise`, `nanosb apply` + `sandbox.yml`) driving libkrun directly.
- **Cloud — node agent:** the cloud API's node agent (`nanosandboxai/cloud`)
  shells out to `nanosb supervise --spec -` with a runtime `SandboxSpec`.
- **Cloud — containerd shim:** `nanosandboxai/nanosandbox-shim` maps an
  OCI bundle / Kubernetes Pod into a `SandboxSpec` and spawns `nanosb supervise`.

Every runtime change — config fields, defaults, the protocol `SandboxSpec`
contract, the VM-boot path, guest-init hooks — MUST satisfy all three:

1. **Never break local usage.** Local inputs (`sandbox.yml`, CLI flags) and the
   `SandboxConfig` defaults must keep working on a personal machine; never
   require a cloud-only input for a local path. Any change to local-visible
   behavior must be intentional and documented.
2. **Never break the cloud paths.** The node agent and the shim build a
   `SandboxSpec` and hand it to `nanosb supervise`; the runtime must honor that
   contract (e.g. `workdir`, `mounts`, `network`, `command`) exactly as the
   local CLI does.
3. **Stay version-compatible across front doors.** A single runtime build must
   be usable by both local and cloud consumers of the same contract — do not
   add a field or behavior only one front door can supply. Prefer additive,
   backward-compatible changes; when the `SandboxSpec` schema changes, update
   `docs/protocol/sandbox-spec.md` + `sandbox-spec.schema.json` and the cloud /
   shim consumers in the same change.

Before finishing any runtime change, exercise it through the **local CLI**
(`nanosb run` / `nanosb supervise`) and state the cloud / shim impact.

## Sources of truth (for keeping docs accurate)

| Concern | Source of truth |
|---------|-----------------|
| CLI commands, flags, subcommands | `src/main.rs` (clap definitions) |
| TUI slash-commands | `src/tui/commands.rs` (`HELP_ENTRIES`) |
| TUI tests (frames/handlers/E2E) | `src/tui/tests.rs` (ratatui `TestBackend`) + `src/tui/vm_test.rs` |
| `sandbox.yml` schema / defaults | `sandbox/crates/sandbox/src/config/file.rs` + `config/mod.rs` |
| Runtime `SandboxConfig` defaults | `runtime/crates/runtime/src/config/mod.rs` |
| Wire protocol | `docs/protocol/protocol.md` + `crates/protocol` |
| Environment variables | `src/deploy.rs`, `src/supervisor/mod.rs`, `src/tui/run.rs`, `sandbox/crates/sandbox/src/agents_registry.rs`, `scripts/install.sh` |
| Install paths / bundles | `scripts/install.sh`, `install-deps/install.sh`, `scripts/package-sdk.sh` |
| Host/shim supervisor entrypoint (`nanosb supervise`) | `src/main.rs` (`cmd_supervise`), `crates/host/src/deploy.rs`, `docs/runtime-capability-contract.md` |
| libkrun fork (pin, commits, init-hook env) | `runtime/scripts/build-libkrun.sh` (`LIBKRUN_SHA`), `runtime/scripts/patches/README.md`, `nanosandboxai/libkrun`@`nanosandbox` |
| Current version + release policy | `VERSION`, `scripts/set-version.sh`, this file ("Versioning and releases") |

## License

Apache-2.0 (see [`LICENSE`](LICENSE)).
