# Epic 1 — Multi-language Sandbox SDK (protocol-first, helper-based)

Status: decisions locked (2026-10-07) — see §0
Date: 2026-10-07
Related: `docs/epics/epic-2-tui-refactor-and-testing.md`, PR #91

## 0. Decisions (locked 2026-10-07)

| # | Decision |
|---|---|
| D1 | **Substrate = a single slim helper** (`nanosb-runtime`). All SDKs are **protocol clients** over its local socket. |
| D2 | **No C FFI in the SDK.** The C ABI is de-scoped and retired; it is not the substrate and is not shipped as an SDK. |
| D3 | **Bundle by default** (macOS arm64) **and** allow an external runtime override (`NANOSB_HOME` / `paths.runtime`). |
| D4 | **Slim helper via feature flags** (one crate, two profiles) — not a separate codebase. |
| D5 | **Pre-sign at package build time**; no install-time codesigning; lazy re-sign fallback at first run. |
| D6 | **Cloud later**: design a `SandboxClient` backend abstraction now; implement only `Local`. |

## 1. Context (verified 2026-10-07)

**Why the helper, not FFI.** The VM cannot be hosted in-process: `hv_vm_create()` on
macOS requires the `com.apple.security.hypervisor` entitlement in a clean
single-threaded process. Our runtime already boots the VM in a subprocess
(`internal-boot-vm`) spawned from a codesigned binary (`NANOSB_BINARY_PATH` or
`current_exe()`). So an "in-process FFI" still shells out — FFI buys no in-process
benefit, adds a hard ABI contract, makes streaming callbacks bespoke per language,
and would require a native addon + codesign per language × platform.

**Runtime artifacts (three linkage models).**

| Artifact | Linkage | Location today |
|---|---|---|
| libkrun | static, linked at build time | compiled into the helper |
| `libkrunfw.5.dylib` | `dlopen` at runtime | `~/.nanosandbox/libs/` or system |
| `gvproxy` | sidecar process | `which gvproxy` or `~/.nanosandbox/bin/` |
| VM host | subprocess (`internal-boot-vm`) | the helper binary |

**microsandbox (the bar):** 5 languages (TS, Rust, Python, Go, Ruby); native
addon bundles the runtime; `local | cloud` backends; an "Agent Client" protocol
for custom integrations.

## 2. Architecture (ADR-1 — accepted)

```
  SDKs (thin protocol clients)                     Helper (the substrate)
  ───────────────────────────                     ──────────────────────
  Rust │ Python │ TS │ Go │ Ruby │ Java/Kotlin │ C#
        └──────────────┬───────────────────┘
                       │  SandboxClient
                       │   ├── Local  → unix socket (NDJSON)  ─┐
                       │   └── Cloud  → HTTPS API (future)     │
                       ▼                                       ▼
                 versioned protocol  ─────────────►  nanosb-runtime (helper)
                                                     ├── supervisor + VM (internal-boot-vm)
                                                     ├── console / logs
                                                     ├── control socket (attach/exec/logs)
                                                     └── deploy planner (mounts, agent cmd)
                                                     + libkrunfw + gvproxy
```

- **All planning stays in the helper** (config building, mount planning, agent
  command, secrets resolution). Clients send intent (a `sandbox.yml` path or a
  config object) and receive results. Clients never reimplement planning.
- **Streaming is the socket stream** — `exec_stream`, `attach`, `logs --follow`
  are all the same transport in every language.
- **Retire the C FFI**: delete `sandbox/crates/sandbox/src/ffi.rs`, the `ffi`
  feature, and the `cbindgen` plan. Nothing in-tree consumes it.

## 3. Goal

Ship **idiomatic SDKs in more languages than microsandbox** on one stable,
helper-based protocol — no native addons, no C ABI, no per-platform signing per
language.

Non-goals: cloud implementation (design only); Windows; snapshots if the effort
is disproportionate (M5 assessment).

## 4. Gap analysis (vs microsandbox)

| Capability | nanosandbox | microsandbox | Target |
|---|---|---|---|
| Languages | 0 shipped | TS, Rust, Python, Go, Ruby (5) | + Java/Kotlin, C#/.NET (7) |
| exec / shell / attach | ✅ (exec/console attach) | ✅ | parity via protocol |
| stdin / tty / timeout | ✅ (agent) | ✅ | parity |
| rlimits | ❌ | ✅ | add (agent + config) |
| guest `fs` API | ❌ | ✅ | add (exec-agent file verbs) |
| `metrics` | ❌ | ✅ | add (host rusage + guest /proc) |
| `logs` (structured, follow) | partial (CLI) | ✅ | add (protocol) |
| secrets API | partial (env) | ✅ | typed protocol API |
| volumes / mounts | ✅ (virtiofs) | ✅ | expose |
| handles (get/list/remove) | partial (CLI ps) | ✅ | add (protocol) |
| detached mode | ✅ (supervisor) | ✅ | expose |
| snapshots / pause-resume | ❌ | ✅ (local) | assess (M5) |
| `modify()` plan | ❌ | ✅ | assess (M5) |
| pre-boot rootfs patches | ❌ | ✅ | assess (M5) |

## 5. Workstreams

### WS1 — `nanosb-runtime` helper (D4)
- Add a Cargo feature `runtime-host`; build `nanosb-runtime` with
  `--no-default-features --features runtime-host`.
- Exclude TUI/`ratatui`/`arboard`/`rusqlite`/`git2`; keep runtime + supervisor +
  `internal-boot-vm` + deploy planner.
- Verify the helper boots a supervised sandbox and exposes the control socket.
- AC: helper builds; `nanosb-runtime run <img>` works; binary is materially
  smaller than the full CLI.

### WS2 — Local protocol (the substrate)
- Formalize the supervisor socket as a **versioned** protocol:
  `hello`/`version`, `create`/`start`/`stop`/`kill`/`remove`, `status`, `list`,
  `get`, `exec` (buffered), `exec_stream`, `attach`, `logs` (follow + cursor),
  `metrics`, `fs` (read/write/list/mkdir/remove/stat), `secrets` (set/list/rm,
  values never persisted), `modify` (plan).
- NDJSON frames, matching the existing `ControlRequest`/`ControlResponse` shape.
- Document it as the **Agent Client protocol** (mirrors microsandbox's naming).
- AC: protocol spec in `docs/`; a reference client exercises every verb.

### WS3 — `SandboxClient` interface + backend abstraction (D6)
- Define the client contract: `builder`, `create`, `get`, `list`, `remove`,
  `start`, `stop`, `kill`, `exec`, `exec_stream`, `shell`, `attach`, `logs`,
  `metrics`, `fs`, `secrets`, `config`, `detached`.
- `Local` backend (socket) implemented; `Cloud` backend stubbed behind the same
  trait (returns `NotImplemented`), so it slots in later without reshaping SDKs.

### WS4 — Core surface expansion (in the helper)
- `fs`: file verbs in the exec-agent over vsock (works without a project mount),
  surfaced through the protocol.
- `metrics`: host VM rusage + guest `/proc` sampled via exec.
- `logs`: structured, followable, cursor-based (reuse `console.log`).
- `secrets`: typed get/set/rm mapped to the existing env delivery; never on disk.
- `rlimits`, `user`, `workdir`, `timeout`: surface via protocol + config.
- `volumes`/`mounts`, `handles`.

### WS5 — Language SDKs (protocol clients only)
Rust, Python, TypeScript/Node, Go, Ruby, **Java/Kotlin**, **C#/.NET**.
Each: idiomatic API, async where native, typed errors, streaming via the socket.
- AC: each passes the shared conformance suite.

### WS6 — Packaging & signing (D3, D5)
- Bundle `nanosb-runtime` + `libkrunfw` + `gvproxy` for macOS arm64 in each
  package (wheel/npm/gem/jar/nuget/crate).
- **Pre-sign** the helper at build time (entitlement applied); lazy re-sign
  fallback on first run if validation fails.
- External override: `NANOSB_HOME` / `paths.runtime` → use an installed runtime.
- "Runtime setup" doc for Linux/CI.

### WS7 — Conformance suite & docs
- One conformance spec, implemented per language: create, exec, stream, shell,
  attach, fs, logs, metrics, secrets, lifecycle, errors, timeouts.
- Quickstarts + API refs + compatibility matrix.

### WS8 — Retire the C FFI (D2)
- Delete `ffi.rs`, the `ffi` feature, and the `cbindgen`/header work.
- Update `sandbox/Cargo.toml` + `lib.rs`.
- AC: `grep -r 'extern "C" fn sandbox_' sandbox/` is empty; workspace builds.

## 6. Acceptance criteria

- AC1 `nanosb-runtime` builds from features; boots + controls a sandbox.
- AC2 Protocol spec published; a reference client passes every verb.
- AC3 Rust/Python/TS/Go/Ruby SDKs pass the conformance suite.
- AC4 Java/Kotlin + C#/.NET SDKs pass the conformance suite (exceeds microsandbox).
- AC5 `exec`, `exec_stream`, `shell`, `attach`, `fs`, `logs`, `metrics` work in
  every binding on a next-mode vanilla image.
- AC6 No binding persists secret values to disk.
- AC7 The C FFI is fully removed; no SDK depends on a native addon.
- AC8 Bundled macOS arm64 install runs with zero extra setup; `NANOSB_HOME`
  override works.

## 7. Milestones

- **M0** Protocol spec + reference client; helper build profile (WS1+WS2).
- **M1** `SandboxClient` + `Local` backend; retire the C FFI (WS3+WS8).
- **M2** Core surface: fs/metrics/logs/secrets/handles (WS4).
- **M3** Python + TypeScript SDKs (WS5).
- **M4** Go + Ruby SDKs (WS5) — matches microsandbox.
- **M5** Java/Kotlin + C#/.NET (WS5) + snapshot/modify assessment.
- **M6** Packaging/signing + conformance across all + docs (WS6+WS7).

## 8. Risks

- R1 **Protocol surface creep** → version it (`hello`/`version`) and keep verbs
  minimal; the helper owns planning.
- R2 **`fs` needs a transport** (agent verbs vs mount) → agent verbs preferred
  (works without a project mount). Decide in M2.
- R3 **Helper bundle size / signing** → feature-gate the helper; pre-sign at
  build; lazy re-sign fallback.
- R4 **7-language packaging sprawl** → automation; ship macOS arm64 first; the
  `NANOSB_HOME` override covers Linux/CI until bundling lands there.
- R5 **Snapshots/modify** may be disproportionate on libkrun → time-boxed M5;
  may defer.

## 9. Open questions

1. Protocol framing: stay NDJSON (matches today) or move to length-prefixed
   binary for high-throughput `exec_stream`? (Recommend NDJSON now; revisit if
   profiling demands it.)
2. `fs` verbs: which operations in v1 (read/write/list/stat/mkdir/remove)?
3. Do we publish the helper as a standalone downloadable runtime in addition to
   bundling?
4. Snapshots / pause-resume: in scope for this epic or a follow-up?
