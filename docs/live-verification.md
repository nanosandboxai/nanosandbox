# Live-VM verification — Epic 1 Multi-language Sandbox SDK

**Date:** 2026-10-12
**Branch:** `feat/epic-1-sdk`
**Test:** `crates/client/tests/live_conformance.rs` (Rust reference, `#[ignore]`-gated) + the per-SDK live E2E below + `scripts/test-packaged-boot.sh`
**Result:** ✅ **PASS** — all 8 bindings boot a real microVM; volumes, disk-only snapshots, and the packaged install are served through the installed helper.

This run is the AC5 / AC6 / AC9 evidence for Epic 1 that requires a **real
microVM** (libkrun + gvproxy + a cached image). It builds/spawns the real
`nanosb-runtime` helper (`serve`), connects `nanosb-client`, and drives the
operations that cannot be faked by the CI conformance harness.

## Prerequisites

| Requirement | Checked | Notes |
|---|---|---|
| macOS Apple Silicon (arm64) | ✅ | `uname -m` → `arm64` |
| `nanosb-runtime` binary | ✅ | `target/debug/nanosb-runtime` (built via `cargo build -p nanosb-cli --bin nanosb-runtime`) |
| libkrun | ✅ | `~/.nanosandbox/lib/libkrun.a` |
| libkrunfw | ✅ | `~/.nanosandbox/libs/libkrunfw.5.dylib` |
| gvproxy | ✅ | `~/.nanosandbox/bin/gvproxy` |
| Hypervisor.framework | ✅ | `nanosb doctor` → 4/4 checks passed |
| Cached image | ✅ | `library/alpine:latest` (4.0 MB, pulled 16 h before run) |

`nanosb doctor` output before the run:

```
[✓] Architecture: Apple Silicon (aarch64)
[✓] libkrunfw Kernel Firmware: found (libkrunfw.5.dylib)
[✓] Hypervisor.framework: available
[✓] gvproxy: available (full outbound networking)
4 checks passed, 0 errors, 0 warnings
```

## Exact command

```bash
cargo build -p nanosb-cli --bin nanosb-runtime --no-default-features --features runtime-host
cargo test -p nanosb-client --test live_conformance -- --ignored --nocapture
```

Optional overrides:

```bash
NANOSB_RUNTIME_BIN=/path/to/nanosb-runtime \
NANOSB_TEST_IMAGE=library/alpine:latest \
  cargo test -p nanosb-client --test live_conformance -- --ignored --nocapture
```

## Result

```
running 1 test
nanosb-runtime: listening on /var/folders/.../nsb-live-51401/helper.sock
live_conformance: exec ok; snapshots create/list/restore/remove ok
test live_conformance_smoke ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.41s
```

A real microVM booted (create returned `Running`, `exec` returned
`hello-from-live`, `metrics.disk_used_bytes > 0`). Cleanup verified: no leftover
sandbox dirs under `~/.nanosandbox/sandboxes/`, no leftover temp dirs, no
leftover volumes.

## What passed (verified against a live VM)

| Area | Verbs exercised | Outcome |
|---|---|---|
| Lifecycle | `create` (boots VM) → `get` → `exec` → `stop` → `remove` → `list` | ✅ |
| Filesystem | `fs_write` / `fs_read_to_string` / `fs_exists` / `fs_mkdir` / `fs_stat` | ✅ |
| Observability | `logs` (non-empty), `metrics` (disk used > 0) | ✅ |
| Secrets (AC7) | `secret_set` / `secret_list` / `secret_remove`; values never returned (`value: None`) | ✅ |
| Pause / resume (AC6) | `pause` (SIGSTOP) → state still `Running` → `resume` (SIGCONT) → `exec` succeeds | ✅ |
| Volumes (AC5) | `volume_create` / `volume_list` / `volume_get` / `volume_remove` **via the sandbox control socket** | ✅ |

## Per-SDK live E2E

Every language SDK ships a `ManagedRuntime` (locate + spawn `nanosb-runtime
serve` on a private socket) and an opt-in live test that drives a **real
microVM** — the same operations as the Rust reference above. Requires macOS
arm64 + libkrun/gvproxy + a cached image; each test is skipped with a clear
reason otherwise.

| SDK | Command | Result |
|---|---|---|
| Rust (reference) | `cargo test -p nanosb-client --test live_conformance -- --ignored` | ✅ PASS |
| Python | `NANOSB_E2E=1 python3 sdk/python/tests/test_e2e.py` | ✅ PASS |
| TypeScript/Node | `NANOSB_E2E=1 node --test sdk/typescript/tests/e2e.test.mjs` | ✅ PASS |
| Go | `NANOSB_E2E=1 go test ./sdk/go/e2e -run TestLiveMicroVMSmoke` | ✅ PASS |
| Ruby | `NANOSB_E2E=1 ruby -I sdk/ruby/lib sdk/ruby/test/test_e2e.rb` | ✅ PASS |
| Java | `NANOSB_E2E=1 mvn -f sdk/java/pom.xml test -Dtest=E2ETest` | ✅ PASS |
| Kotlin | `NANOSB_E2E=1 java -cp sdk/kotlin/nanosandbox-kotlin.jar nanosandbox.kt.E2EKt` | ✅ PASS |
| C#/.NET | `NANOSB_E2E=1 dotnet test sdk/dotnet --filter E2ETest` | ✅ PASS |

Each test boots a real microVM and asserts: create→running, get, exec stdout,
`exec_stream` (streamed stdout + exit code), `shell` (buffered script),
`attach` (open the console session), fs write/read/exists/mkdir/stat, logs
non-empty, metrics disk>0, list contains, secret set/list (value
redacted)/remove, pause→running→resume→exec, volume create/list/remove, snapshot
create/list/restore/remove, stop, remove. See
`docs/plans/2026-10-11-sdk-live-e2e-plan.md`.

All eight were re-run and recorded **PASS** on macOS arm64 against a real
microVM on 2026-10-12 (Rust reference 5.47 s; Python 4.94 s; TS 5.10 s; Go
5.03 s; Ruby `1 runs, 29 assertions, 0 failures`; Java `Tests run: 1, Failures:
0, Errors: 0`; Kotlin `PASS kotlin live microVM smoke`; C# `Passed! - Failed: 0,
Passed: 1`). The Rust reference test also tears down the `*-restored` sandbox it
creates, so a run leaves no sandbox dir or VM process behind.

## Packaged install (AC9)

`scripts/test-packaged-boot.sh` proves the WS9 bundle is self-contained:

```bash
scripts/test-packaged-boot.sh
```

It runs `package-sdk.sh` → the bundle's `install.sh` into a **fresh**
`NANOSB_HOME` → verifies the installed helper is code-signed → boots a real
microVM through the **installed** helper (`NANOSB_RUNTIME_BIN` unset, so
`ManagedRuntime` resolves `$NANOSB_HOME/bin/nanosb-runtime`). The bundle ships
the helper, `gvproxy`, `libkrunfw.5.dylib`, **and** the guest `exec-agent`
(`$NANOSB_HOME/guest-bin/exec-agent`); the supervisor and client resolve the
sandbox dir and guest bin from `NANOSB_HOME`.

```
==> 3/5 verify the installed helper is code-signed
/tmp/nsb9.XXXXXX/nanosandbox-home/bin/nanosb-runtime: satisfies its Designated Requirement
==> 5/5 boot a real microVM through the INSTALLED helper
Ran 1 test in 5.322s
OK
PASS: packaged install boots a microVM with zero extra setup (AC9)
```

## Blockers (precise)

### 1. Volumes — FIXED

The helper daemon now proxies `VolumeCreate`/`VolumeList`/`VolumeGet`/
`VolumeRemove`/`MountAdd` to the supervisor (mirroring the secret verbs) and
advertises `Capability::Volumes`. `volume_create` → `volume_list` →
`volume_get` → `volume_remove` round-trips cleanly through the helper socket.
(Fixed in `src/bin/nanosb_runtime.rs`; the supervisor already implemented the
registry.)

### 2. Snapshots — disk-only (AC6)

The helper daemon now serves the snapshot verbs from a disk-only store
(`src/supervisor/snapshots.rs`; see `docs/decisions/snapshots.md`) and
advertises `Capability::Snapshots`: `snapshot_create` → `snapshot_list` →
`snapshot_restore` (materialises a new, independent sandbox directory) →
`snapshot_remove` round-trips through the helper socket. Memory/process capture
and booting a VM on the restored writable layer are out of scope (epic R3).

## AC mapping

| AC | Evidence |
|---|---|
| AC5 (exec/exec_stream/shell/attach/fs/logs/metrics/volumes in every binding) | ✅ live in all 8 bindings on a next-mode vanilla image: exec/exec_stream/shell/attach + fs/logs/metrics/volumes all pass |
| AC6 (snapshot + restore, pause + resume) | pause/resume ✅ live in all 8; disk-only snapshot create/list/restore/remove ✅ live in all 8 (memory capture deferred under R3) |
| AC9 (bundled macOS arm64, `NANOSB_HOME`) | ✅ `scripts/test-packaged-boot.sh` — bundle install into a fresh `NANOSB_HOME` boots a real microVM; helper + `gvproxy` + `libkrunfw` + guest `exec-agent` all resolved from `NANOSB_HOME` |

## Re-running

All live tests are opt-in / `#[ignore]`-gated so CI stays green without a VM.
Re-run any of them with the exact command in the matrix above, and re-run the
packaged-install check with `scripts/test-packaged-boot.sh`.

## Epic 7 — Docker-in-VM (Tier 1)

Verified on macOS arm64 (next mode) on 2026-10-19 with a Docker-enabled agent
image (`claude-docker`) cached from a local registry.

```bash
NANOSB_RUNTIME_BIN="$PWD/target/debug/nanosb-runtime" \
NANOSB_TEST_DOCKER_IMAGE=localhost:5050/claude-docker:latest \
  cargo test -p nanosb-client --test epic7_docker_live -- --ignored --nocapture
```

Observed inside the microVM:

- `/dev/vda` (virtio-blk serial `disk0`) formatted ext4 and mounted at
  `/var/lib/docker`; `df` shows the 4 GiB data disk.
- `docker info`: `Storage Driver: overlay2`, `Cgroup Version: 2`,
  `Docker Root Dir: /var/lib/docker`.
- `docker run --rm --network host hello-world` → exit 0, "Hello from Docker!".

Tier 1 has no default bridge; use `--network host` for containers that need
outbound networking. Bridge/NAT and `-p` publishing require Epic 8.
