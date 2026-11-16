# Plan — real end-to-end (live microVM) verification for every SDK

Status: active (2026-10-11)
Related: `crates/client/tests/live_conformance.rs` (Rust reference), `docs/live-verification.md`, `docs/epics/epic-1-multi-language-sdk.md` (AC5/AC6).

## Goal

Prove that **every** SDK — Python, TypeScript/Node, Go, Ruby, Java, C#/.NET,
Kotlin — drives a **real microVM through the nanosandbox runtime**, behaving
exactly like the Rust reference client. The conformance suite proves protocol
correctness against a scripted server; this proves the real end-to-end path:

```
SDK client ──NDJSON/Unix socket──▶ nanosb-runtime (helper) ──▶ supervisor ──▶ libkrun ──▶ microVM
```

## Prerequisite: provide the runtime to every SDK

SDKs are protocol clients; they need the helper (`nanosb-runtime`) running.
Each SDK gains a small **`ManagedRuntime`** helper that:

1. Locates the helper: `NANOSB_RUNTIME_BIN` env → `$NANOSB_HOME/bin/nanosb-runtime`
   → `PATH` → repo `target/debug/nanosb-runtime`.
2. Spawns `nanosb-runtime serve --socket <tmp>/helper.sock`.
3. Waits for the socket to appear (bounded), then returns its path.
4. On close: kills the helper and removes the temp dir.

This is the client-side half of the WS9 packaged bundle (Epic 4): with a bundled
helper, `ManagedRuntime` gives zero-config "manage a real sandbox" from any SDK.

## Per-SDK deliverables

For **each** of the seven non-Rust SDKs:

1. `ManagedRuntime` (idiomatic per language): locate/spawn/wait/teardown.
2. An **opt-in E2E test** (env-gated, e.g. `NANOSB_E2E=1`, and skipped with a
   clear reason when the VM stack is absent) that mirrors the Rust smoke:
   - `create` boots a real microVM → `state == running`
   - `get` → name/state/image
   - `exec echo hello-from-live` → exit 0, stdout matches
   - `fs_write` / `fs_read_to_string` / `fs_exists` / `fs_mkdir` / `fs_stat`
   - `logs(10)` non-empty; `metrics().disk_used_bytes > 0`
   - `list` contains the sandbox
   - `secret_set` / `secret_list` (value redacted) / `secret_remove`
   - `pause` (state still running) → `resume` → `exec` succeeds
   - `volume_create`/`list`/`get`/`remove` (helper advertises Volumes)
   - `snapshot_create`/`list`/`restore`/`remove` (helper advertises Snapshots)
   - `stop` → `remove` → gone from `list`
3. A recorded result in `docs/live-verification.md` (per language).

The E2E is **not** run in default CI (needs macOS arm64 + libkrun/gvproxy + a
cached image); it is `#[ignore]`/env-gated and runnable on demand.

## Verification matrix

| SDK | Launcher | E2E entry point | Run command | Result |
|---|---|---|---|---|
| Rust (reference) | `start_daemon()` | `live_conformance_smoke` | `cargo test -p nanosb-client --test live_conformance -- --ignored` | **PASS** |
| Python | `nanosandbox.runtime.ManagedRuntime` | `sdk/python/tests/test_e2e.py` | `NANOSB_E2E=1 python3 sdk/python/tests/test_e2e.py` | **PASS** |
| TypeScript | `runtime.mjs ManagedRuntime` | `sdk/typescript/tests/e2e.test.mjs` | `NANOSB_E2E=1 node --test sdk/typescript/tests/e2e.test.mjs` | **PASS** |
| Go | `StartRuntime` (`sdk/go/nanosandbox/runtime.go`) | `sdk/go/e2e/e2e_test.go` | `NANOSB_E2E=1 go test ./sdk/go/e2e -run TestLiveMicroVMSmoke` | **PASS** |
| Ruby | `Nanosandbox::ManagedRuntime` | `sdk/ruby/test/test_e2e.rb` | `NANOSB_E2E=1 ruby -I sdk/ruby/lib sdk/ruby/test/test_e2e.rb` | **PASS** |
| Java | `nanosandbox.ManagedRuntime` | `sdk/java/src/test/java/nanosandbox/E2ETest.java` | `NANOSB_E2E=1 mvn -f sdk/java/pom.xml test -Dtest=E2ETest` | **PASS** |
| Kotlin | `nanosandbox.ManagedRuntime` | `sdk/kotlin/e2e/E2E.kt` | `NANOSB_E2E=1 java -cp sdk/kotlin/nanosandbox-sdk-kotlin.jar nanosandbox.kt.E2EKt` | **PASS** |
| C#/.NET | `ManagedRuntime` | `sdk/dotnet/tests/E2ETest.cs` | `NANOSB_E2E=1 dotnet test sdk/dotnet --filter E2ETest` | **PASS** |

All eight verified on macOS arm64 against a real microVM (2026-10-11).

## Shared contract (so every SDK behaves identically)

- Helper socket env override: `NANOSB_RUNTIME_BIN`.
- Image: `NANOSB_TEST_IMAGE` (default `library/alpine:latest`).
- Sandbox name: `sdk-e2e-<pid>`.
- Timeout: create waits up to 60 s for `running`.
- Teardown always: stop → remove → kill helper → delete temp dirs and the
  sandbox dir (no leaked VM processes or dirs).

## Acceptance criteria

- **AC-E1** Each of the seven SDKs ships a `ManagedRuntime` that starts and stops
  the real helper.
- **AC-E2** Each SDK's E2E boots a real microVM and asserts the full op list,
  matching the Rust reference outcomes.
- **AC-E3** Each E2E is opt-in and **skips with a clear reason** (never silently
  passes) when the helper/VM/image is unavailable.
- **AC-E4** Each E2E is run and recorded PASS in `docs/live-verification.md`,
  with the exact command.
- **AC-E5** No leaked resources after a run: no helper process, no sandbox dir,
  no temp dir.

## Risks

- **R1** Kotlin/Java/.NET E2E frameworks need the VM stack on the runner — keep
  them opt-in.
- **R2** Image pull needs network on first run — use a cached image
  (`NANOSB_TEST_IMAGE`).
- **R3** VM boot is slow (~5 s) and can flake — bounded waits + retry on the
  socket, fail loudly otherwise.
- **R4** Each language spawns a child process differently — the launcher must be
  robust (kill process group where needed).
