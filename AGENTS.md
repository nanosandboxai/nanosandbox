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

# TUI/CLI unit tests (no TTY, no VM) — runs in CI
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

# Local PTY smoke harness
scripts/tui-smoke.sh
```

## Conventions

- **Rust edition 2021.** Match the existing style in the file you are editing.
- **Single version source of truth.** The workspace version lives in the root
  `Cargo.toml` `[workspace.package] version`; member crates use
  `version.workspace = true`. Exception: `guest/crates/exec-agent` is excluded
  from the workspace (linux-only, cross-compiled) and carries its own version.
- **Never suppress type errors** (`as any`, `@ts-ignore`, `unwrap_or_default`
  used to hide a real error, etc.). Fix the cause.
- **Keep changes minimal and focused.** Do not refactor unrelated code in the
  same change. Fix bugs minimally; never refactor while fixing.
- **Atomic commits.** One logical change per commit, with a conventional-commit
  subject (`feat:`, `fix:`, `chore:`, `docs:`, `ci:`) matching existing history.
- Never commit unless the user asked for it.

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
| `sandbox.yml` schema / defaults | `sandbox/crates/sandbox/src/config/file.rs` + `config/mod.rs` |
| Runtime `SandboxConfig` defaults | `runtime/crates/runtime/src/config/mod.rs` |
| Wire protocol | `docs/protocol/protocol.md` + `crates/protocol` |
| Environment variables | `src/deploy.rs`, `src/supervisor/mod.rs`, `src/tui/run.rs`, `sandbox/crates/sandbox/src/agents_registry.rs`, `scripts/install.sh` |
| Install paths / bundles | `scripts/install.sh`, `install-deps/install.sh`, `scripts/package-sdk.sh`, `scripts/sdk/build-runtime-bundle.sh` |
| Host/shim supervisor entrypoint (`nanosb supervise`) | `src/main.rs` (`cmd_supervise`), `crates/host/src/deploy.rs`, `docs/runtime-capability-contract.md` |
| libkrun fork (pin, commits, init-hook env) | `runtime/scripts/build-libkrun.sh` (`LIBKRUN_SHA`), `runtime/scripts/patches/README.md`, `nanosandboxai/libkrun`@`nanosandbox` |
| SDK version (single source of truth) | `VERSION` + `scripts/set-sdk-version.sh` (`--check` = drift gate) |
| SDK publish pipeline | `.github/workflows/sdk-{ci,release,smoke}.yml` + `scripts/sdk/{check-published,publish,install-smoke}.sh` |
| SDK runtime provisioner | `scripts/sdk/ensure-runtime.sh` (embedded in each SDK) |
| Per-platform runtime bundle | `nanosandbox-runtime-<os>-<arch>.tar.gz` under the `v<VERSION>` release (layout below) |

## SDK runtime provisioning (MANDATORY)

The nanosandbox runtime is **auto-provisioned by each SDK** — installing the SDK
is the only step a user needs. `ManagedRuntime` resolves the `nanosb-runtime`
helper (`NANOSB_RUNTIME_BIN` → `$NANOSB_HOME/bin/nanosb-runtime` → `PATH` → the
repo `target/debug/nanosb-runtime`) and, if it is absent, runs the shared
provisioner to download, verify, and install it. Rules:

1. **No separate runtime install step.** Each SDK's `ManagedRuntime` provisions
   the runtime itself on first use. The package-manager install of the SDK is the
   only step.
2. **The provisioner is a single source of truth, embedded in every SDK.**
   [`scripts/sdk/ensure-runtime.sh`](scripts/sdk/ensure-runtime.sh) is the
   canonical provisioner. Its content is **embedded byte-identical** in every
   SDK so auto-provisioning works from an installed package with no repo present.
   Any change to it MUST be re-synced into every embed (see "7 embed locations"
   below).
3. **Version is pinned to the SDK release.** The runtime version is the SDK's
   release version (root [`VERSION`](VERSION)); the provisioner downloads
   `v${NANOSB_VERSION}/nanosandbox-runtime-<os>-<arch>.tar.gz` and verifies it
   with `NANOSB_RUNTIME_SHA256`.
4. **The release MUST publish per-platform runtime bundles.** Every release must
   attach `nanosandbox-runtime-<os>-<arch>.tar.gz` for each supported platform.
   Bundle layout: `bin/nanosb-runtime`, `bin/gvproxy`, `libs/libkrunfw*.dylib`,
   `guest-bin/exec-agent`, and `entitlements.plist`.
5. **macOS: re-sign with the hypervisor entitlement.** After unpacking, the
   provisioner ad-hoc re-signs `nanosb-runtime` with the
   `com.apple.security.hypervisor` entitlement (from `entitlements.plist`) so
   Hypervisor.framework works.

Config comes from the environment: `NANOSB_HOME` (install root), `NANOSB_VERSION`
(version to download), `NANOSB_RUNTIME_URL` (release base URL), `NANOSB_OFFLINE=1`
(never download), `NANOSB_RUNTIME_SHA256` (expected sha256), `NANOSB_PROVISIONER`
(explicit provisioner path).

### The 7 embed locations

The provisioner is embedded in these files, one per SDK. Keep them in sync with
`scripts/sdk/ensure-runtime.sh`:

| SDK | Embed location |
|-----|----------------|
| Rust | `sdk/rust/src/ensure_runtime.sh` (included by `sdk/rust/src/managed_runtime.rs`) |
| Python | `sdk/python/nanosandbox/runtime.py` |
| TypeScript/Node | `sdk/typescript/nanosandbox/runtime.mjs` |
| Go | `sdk/go/nanosandbox/runtime.go` |
| Ruby | `sdk/ruby/lib/nanosandbox/runtime.rb` |
| Java | `sdk/java/src/main/java/nanosandbox/ManagedRuntime.java` |
| C#/.NET | `sdk/dotnet/nanosandbox/ManagedRuntime.cs` |

(Kotlin reuses the Java SDK's `ManagedRuntime`, so it has no separate embed.)

## SDK versioning & publishing

The eight language SDKs share one version, held in the root [`VERSION`](VERSION)
file. `scripts/set-sdk-version.sh <semver>` propagates it to every manifest
(root `Cargo.toml`, `Cargo.lock` workspace members, Python, TypeScript, Java,
Ruby gemspec, .NET csproj, Kotlin Gradle); `--check` fails on drift and runs as
the `version-drift` gate in CI. Never hand-edit a single manifest.

Publishing is tag-driven and independent of the product `v*` release:

- `.github/workflows/sdk-ci.yml` — PR gate: the conformance harness artifact,
  one job per language (unit + conformance + dry-run package), and `version-drift`.
- `.github/workflows/sdk-release.yml` — `sdk-v*` tag (or `workflow_dispatch`
  with `dry_run`) publishes every SDK behind the `sdk-release` environment using
  registry token secrets: `CARGO_REGISTRY_TOKEN` (crates.io), `PYPI_API_TOKEN`
  (PyPI), `NPM_TOKEN` (npm), `RUBYGEMS_API_KEY` (RubyGems), `NUGET_API_KEY`
  (NuGet), Go via a `sdk/go/v*` tag, and Java + Kotlin to Maven Central
  (`MAVEN_TOKEN` plus a mandatory GPG key via `MAVEN_GPG_PRIVATE_KEY` /
  `MAVEN_GPG_PASSPHRASE`).
  `scripts/sdk/publish.sh` skips already-published versions (idempotent).
- `.github/workflows/sdk-smoke.yml` — installs each published SDK from its real
  package manager in a scratch environment (`scripts/sdk/install-smoke.sh`).

Each SDK **auto-provisions the runtime** on first use (see "SDK runtime
provisioning" above), so publishing an SDK is the only delivery step — no
separate runtime install is shipped or expected. The runtime version is pinned
to the SDK release version (`VERSION`); the release pipeline must therefore also
publish the per-platform `nanosandbox-runtime-<os>-<arch>.tar.gz` bundle at
`v${NANOSB_VERSION}` for that version to be resolvable.

Registry targets: Rust/Python/npm/Ruby as `nanosandbox-sdk`; .NET as
`Nanosandbox.Sdk`; Go module path unchanged; Java
`ai.nanosandbox:nanosandbox-sdk`, Kotlin `ai.nanosandbox:nanosandbox-sdk-kotlin`.

## License

Apache-2.0 (see [`LICENSE`](LICENSE)).
