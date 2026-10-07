# Epic 1 — Multi-language Sandbox SDK (match and exceed microsandbox)

Status: draft
Date: 2026-10-07
Related: `runtime/docs/zero-image-customization-security.md`, PR #91

## 1. Context (verified 2026-10-07)

**What nanosandbox has today**
- Rust SDK crate (`sandbox`) + a C ABI (`sandbox/crates/sandbox/src/ffi.rs`,
  `--features ffi`). **15 exported symbols:**
  `last_error`, `sandbox_create`, `sandbox_start`, `sandbox_stop`,
  `sandbox_destroy`, `sandbox_free`, `sandbox_status`, `sandbox_exec`,
  `sandbox_exec_stream`, `image_pull`, `image_list`, `image_exists`,
  `free_string`, `version`, `validate_runtime`.
- Exec now works in next mode over virtio-vsock (`ExecClient`).
- **No shipped language bindings.** Python/Node/Go bindings were planned but are
  not in-tree; the ABI has no consumers we can see.

**What microsandbox ships (the bar to clear)**
- **Languages (5): TypeScript, Rust, Python, Go, Ruby.**
- Surface: `exec`/`execWith`/`execStream`/`execStreamWith`, `shell`/`shellStream`,
  `attach`/`attachWith`/`attachShell`; stdin (pipe/bytes), `tty`, `rlimit`,
  `timeout`, `user`, `workdir`, `env`; `fs()` (guest filesystem); `metrics()` +
  `metricsStream()`; `logs()` + `logStream()` (on-disk JSON Lines, `follow`,
  `cursor`); secrets; volumes (bind/named/tmpfs/disk); network + published ports;
  snapshots + pause/resume (local-only); `modify()` (live / next-start /
  requires-restart plan); labels; `Sandbox.get/list/listWith/remove/start/
  startDetached`; detached mode; pull policy; registry config; pre-boot rootfs
  patches.
- Design notes: the SDK **spawns the VM directly as a child process** (no
  daemon), or targets microsandbox cloud with an API key. A low-level "Agent
  Client" protocol is documented for custom integrations.

## 2. Gap analysis

| Capability | nanosandbox | microsandbox | Epic target |
|---|---|---|---|
| Rust SDK | ✅ | ✅ | parity |
| Python | ❌ | ✅ | ship |
| TypeScript/Node | ❌ | ✅ | ship |
| Go | ❌ | ✅ | ship |
| Ruby | ❌ | ✅ | ship |
| **Java/Kotlin** | ❌ | ❌ | **exceed** |
| **C#/.NET** | ❌ | ❌ | **exceed** |
| **C (ABI)** | ✅ | ❌ (not advertised) | keep + document |
| exec / shell / attach | ✅ (exec/shell; attach via console) | ✅ | parity |
| stdin / tty / rlimit / timeout | ✅ (stdin/tty/timeout) | ✅ | add rlimits |
| guest filesystem (`fs`) | ❌ | ✅ | add |
| metrics / logs API | ❌ (CLI logs only) | ✅ | add |
| secrets API | partial (env delivery) | ✅ | add |
| volumes / mounts | ✅ (virtiofs) | ✅ | expose in SDK |
| snapshots / pause-resume | ❌ | ✅ (local) | assess (may defer) |
| `modify()` plan | ❌ | ✅ | assess |
| handles (`get/list/remove`) | partial (CLI `ps`) | ✅ | add |
| detached mode | ✅ (supervisor) | ✅ | expose |
| pre-boot rootfs patches | ❌ | ✅ | assess |

## 3. Goal

Ship **first-class, idiomatic SDKs in more languages than microsandbox**, sharing
one stable core, so nanosandbox is the most language-accessible microVM sandbox.

Non-goals: a hosted cloud backend; Windows; re-implementing snapshots if the
effort is disproportionate (assessed in M5).

## 4. Architecture decision (ADR-1): core + thin bindings

Two viable cores; **recommend A**:

- **A. Expand the C ABI, then generate/bind per language.** One `libnanosandbox_sdk`
  (`cdylib`) + a stable header; each language binds via FFI (ctypes, N-API, cgo,
  FFI gem, JNI, P/Invoke). Pros: single implementation, no IPC, matches the
  existing `--features ffi`. Cons: callbacks for streaming are clunky across
  languages; error strings need marshalling.
- **B. A stable local agent protocol** (JSON-over-unix-socket / IPC), with thin
  language clients — closer to microsandbox's "Agent Client". Pros: trivial
  bindings, streaming is natural, language-agnostic; reuses the supervisor
  control socket. Cons: a separate process model to document.

Recommended: **A for the library surface + B for streaming/attach** — expose a
synchronous/FFI core for lifecycle + exec, and a documented local socket protocol
(the supervisor control socket, already NDJSON) for streaming/attach/logs. This
gives cheap, idiomatic bindings without FFI callback contortions.

## 5. Workstreams

### WS0 — ABI stabilization & versioning
- Freeze `sandbox_*` semantics; add `sandbox_abi_version()`.
- Define memory/ownership rules (who frees, thread-safety, error retrieval).
- Provide a generated C header (`cbindgen`) checked into the repo.

### WS1 — Core surface expansion (the missing primitives)
In the `sandbox` crate (not just FFI), so Rust SDK users get them too:
- `fs`: read/write/list/mkdir/remove/stat for the guest filesystem. **Needs a
  transport** — either extend the exec agent (a file-ops verb set over vsock) or
  use a shared virtiofs mount. Prefer agent verbs (works without a project mount).
- `metrics`: CPU/mem/IO. Host-side where possible (VM rusage) + guest `/proc` via
  exec for accuracy.
- `logs`: expose the supervisor `console.log` as a structured, followable stream
  (JSON Lines + cursor), reusing the supervisor control socket.
- `secrets`: a typed API that maps to the existing env delivery + a scoped
  secret store; never persist values.
- `volumes`/`mounts`: expose `SandboxConfig.mounts`/`extra_mounts` in the SDK.
- `handles`: `get`/`list`/`remove` over the supervisor registry + `state.json`.
- `rlimits`, `user`, `workdir`, `timeout`: already in config; surface them.

### WS2 — Language bindings
For each language: idiomatic ergonomics, async where native, typed errors, and a
shared conformance suite.
- **Rust** (in-tree crate) — reference.
- **Python** (`nanosandbox` on PyPI; ctypes/cffi over the ABI; async).
- **TypeScript/Node** (npm; N-API addon; promises + async iterators).
- **Go** (module; cgo; context-based).
- **Ruby** (gem; FFI; blocks for streaming).
- **Java/Kotlin** (Maven Central; JNI or JNA; **exceeds microsandbox**).
- **C#/.NET** (NuGet; P/Invoke; `IAsyncEnumerable`; **exceeds microsandbox**).
- **C** (documented ABI + header; examples).

### WS3 — Packaging & release
- Per-language CI build + publish (PyPI, npm, crates.io, Go module tag, RubyGems,
  Maven Central, NuGet).
- Ship the prebuilt runtime libs (libkrun/libkrunfw/gvproxy) or document
  `Runtime setup` like microsandbox.

### WS4 — Streaming & attach protocol
- Formalize the supervisor control socket (`ControlRequest`/`ControlResponse`,
  `AttachFrame`) as a versioned, documented protocol usable by any language
  without FFI. This is what makes `attach`/`logs --follow` trivial everywhere.

### WS5 — Conformance & docs
- One **conformance test suite** (spec) exercised by every binding: create, exec,
  stream, shell, attach, fs, logs, metrics, secrets, lifecycle, error cases.
- Per-language quickstarts + API reference; a compatibility matrix.

## 6. Acceptance criteria

- AC1 Rust/Python/TypeScript/Go/Ruby SDKs each pass the conformance suite.
- AC2 Java/Kotlin and C#/.NET SDKs each pass the conformance suite (exceeds
  microsandbox's language count).
- AC3 `exec`, `exec_stream`, `shell`, `attach` work in every binding on a
  next-mode vanilla image.
- AC4 `fs` read/write/list and `logs` follow work in every binding.
- AC5 Every binding exposes typed errors and a documented timeout/abort.
- AC6 `cargo test -p sandbox --features ffi` green; header generated and stable.
- AC7 No binding persists secret values to disk (matches the security model).

## 7. Milestones

- **M0** ADR-1 decision + ABI freeze + generated header.
- **M1** Rust SDK covers the full surface (fs/logs/metrics/secrets/volumes/handles).
- **M2** Streaming/attach protocol documented + a reference client.
- **M3** Python + TypeScript (the two most-requested).
- **M4** Go + Ruby (match microsandbox's set).
- **M5** Java/Kotlin + C#/.NET (exceed) + snapshot/modify assessment.
- **M6** Conformance suite across all bindings + docs + release automation.

## 8. Risks

- R1 **Streaming FFI callbacks are awkward** → mitigated by ADR-1 option B.
- R2 **`fs` transport choice** (agent verbs vs mount) affects non-project use →
  decide in M1; agent verbs preferred.
- R3 **Packaging/release sprawl** (7 languages) → automation or a subset first.
- R4 **Snapshot/pause-resume** may be disproportionate on libkrun →
  time-boxed assessment in M5; may defer.

## 9. Open questions

1. ADR-1: FFI-first (A) vs protocol-first (B) — or the recommended hybrid?
2. Which languages first (recommend Python, TypeScript, then Go/Ruby)?
3. Guest filesystem API over agent verbs or a shared mount?
4. Do we ship the runtime binaries in packages, or require `Runtime setup`?
5. Are snapshots/pause-resume in scope for this epic or a follow-up?
