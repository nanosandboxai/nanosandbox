# Runtime & Capability Contract (for host-side consumers / shim authors)

This document is the contract a host-side consumer (e.g. the containerd shim)
relies on when talking to the Nanosandbox runtime helper. It is part of
Epic 10 (#168).

## 1. Protocol version

The wire protocol is versioned and implemented by the `nanosb-protocol` crate:

- **Current version:** `1.0.0` (`nanosb_protocol::PROTOCOL_VERSION`).
- Every connection begins with a **`hello`** handshake. The helper replies with
  its agreed protocol version and the capability list it implements.
- Version compatibility helpers: `nanosb_protocol::{cmp_version, version_in_range}`.

Clients **must** negotiate before issuing any optional verb.

## 2. Capabilities

Capabilities are declared in `nanosb_protocol::hello::Capability`. A consumer
must check that a capability is present before calling the verbs it gates
("no half-implemented verb set").

| Capability | Gates |
|-----------|-------|
| `lifecycle` | create/start/stop/kill/remove/get/list/modify/drain/touch/ping |
| `exec` | exec / exec_stream / shell / attach |
| `fs` | `fs/*` verbs |
| `logs` | logs (+ follow + cursor) |
| `metrics` | metrics / metrics_stream |
| `volumes` | `volumes/*` verbs |
| `secrets` | `secrets/*` verbs |
| `snapshots` | `snapshot/*` + pause/resume |
| `networking` | publish TCP/UDP ports + policy passthrough |
| `handles` | labels + handles |
| `ephemeral` | ephemeral sandboxes |
| `signal` | `kill` with an explicit signal (SIGTERM/SIGKILL) |
| `wait` | blocking `wait` verb (returns exit code) |
| `exec_pty` | `exec_stream` with a PTY + live stdin/resize frames |
| `docker` | helper advertises Docker-in-VM config support; clients gate on it. Container operations use `exec`; bridge networking is Tier 2 |

`ExecPty`, `Signal`, and `Wait` are the shim-facing surface used by
containerd/CRI integration on Linux/KVM.

`exec` and `exec_pty` are advertised **only when the exec channel is configured**
(the resident agent is up); a consumer should treat their absence as "no exec
here" rather than retrying. See §3.3.

## 3. Process contract

- The runtime helper binary is **`nanosb-runtime`** (`src/bin/nanosb_runtime.rs`).
  It serves the protocol on a local socket and is the only process a consumer
  needs to speak to.
- The **supervisor** for a sandbox is a separate process; control flows over
  `$NANOSB_HOME/<sandbox>/control.sock` (see `supervisor::client`).

### 3.1 Supported supervisor entrypoint — `nanosb supervise`

`nanosb supervise --name <n> --spec <SandboxSpec.json|->` is the supported,
stable way for a host-side consumer (the shim) to spawn a supervisor. It performs
the same pre-spawn wiring as the helper:

- stages the guest `exec-agent` into `<sandbox_dir>/agent` (virtiofs read-write at
  `/agent`),
- sets `vsock_socket = <sandbox_dir>/exec.sock` and `vsock_port = 1024`
  (`EXEC_VSOCK_PORT`),
- runs the guest `exec-agent` **resident alongside the workload** (Epic 178):
  when the spec has no `command` the agent itself is PID 1; when it has a
  workload command the workload stays PID 1 and the runtime exports
  `KRUN_EXEC_AGENT=/agent/exec-agent:<port>` so the guest init backgrounds the
  agent. Either way `exec` / `exec_pty` work. See §3.3.

**`--foreground`** spawns the supervisor as a **live child of the caller** (no
`setsid`, not re-parented) so the caller owns and reaps it:

- It reports only once the sandbox reaches `Running` (readiness handshake; typed
  failure on `Error`/timeout), removing the create→start race.
- It writes `<sandbox_dir>/supervisor.pid` (the child pid) and prints the pid.
- When the supervisor exits, the command exits with the **same code**; a signal
  death maps to `128 + signal` (the #164 contract).

Without `--foreground` the supervisor is spawned **detached** (`setsid`; the child
is re-parented to init) and the command prints the pid and returns immediately —
use this when the caller does not need to own the process.

### 3.2 Non-OCI rootfs (containerd snapshotter mounts)

A `SandboxSpec.rootfs` of `{ "kind": "local_dir", "path": "<dir>" }` (or
`"kind": "disk_image"` with `path`/`format`) boots **directly from that host path** — no
OCI image pull and no OCI bundle. This is how a containerd snapshotter mount is
handed to the VM. `start`, `start_next`, and `restart` all support it; an OCI
rootfs still uses the bundle prepared by `create`.

### 3.3 Resident exec agent and workload stdin (Epic 178)

The exec channel is served by the guest `exec-agent` (a vsock server on port
1024, bridged to `<sandbox_dir>/exec.sock`). It runs **resident alongside the
PID-1 workload**, so exec works for a normal container that supplies a command
(`/bin/echo`, `/bin/sleep`, …), not only for the no-command case:

- The runtime exports `KRUN_EXEC_AGENT=/agent/exec-agent:<port>` in the next-mode
  env; the libkrun fork's init backgrounds the agent (it detaches from PID 1's
  session and silences its stdio) and PID 1 reaps it if it dies.
- `hello` advertises `exec` / `exec_pty` **only when the exec channel is
  configured** (`SandboxConfig.vsock_socket`), so a client can fail loudly instead
  of hitting a reset.
- The host exec client retries a new exec while the socket exists and the agent is
  still binding, so a freshly booted sandbox is reliable.

**Workload stdin** (the main process, not `exec`) is fed to the guest console:
write `attach_input` frames on the `attach` stream — the supervisor forwards them
to the guest console stdin (`Ctx.stdin_writer`), which the guest init wires to
PID 1's stdin. A consumer that needs container stdin (e.g. `ctr run --with-stdin`
or `-t`) bridges containerd's stdin FIFO to `attach_input`.

> **Limitation:** there is no stdin **EOF/close** verb. A consumer can feed bytes
> but cannot signal end-of-input, so a workload that reads until EOF (e.g.
> `cat`) does not terminate. Closing the guest console stdin needs a new
> console-stdin close frame.

## 4. Dependency contract (layering)

A host-side consumer must be able to depend on the **thin host layer** without
pulling in the TUI or the VM engine (libkrun FFI):

```
nanosb-protocol  (pure wire types)
  └─ nanosb-host (spec→config, deploy, supervisor client/state; no TUI, no FFI)
       └─ nanosb-cli (adds TUI)   ·   runtime (engine, libkrun FFI)
```

Target consumer dependencies: **`nanosb-protocol` + `nanosb-host` only**.
`nanosb-cli` must not be a dependency (it drags in ratatui/tui-term/arboard and
`runtime → libkrun-sys → krun.a`).

### Status (Epic 10 progress)

- ✅ **H1** — `runtime::config` types extracted into the FFI-free
  `runtime-config` crate (`runtime/crates/runtime-config`); `runtime` re-exports
  it as `runtime::config`. Image normalization (`normalize_image`) also moved there.
- ✅ **H2/H3** — `nanosb-host` (`crates/host`) provides the supervisor client +
  state, `spec::spec_to_config` (+ `mount_spec_to_mount`), and the FFI-free
  `deploy` helpers (`EXEC_VSOCK_PORT`, `stage_exec_agent`, `Origin`,
  `extract_boot_env`). `nanosb-cli` re-exports them for compatibility. The
  engine-coupled `sandbox` (`AgentSandbox`, `runtime::Sandbox`) stays in the CLI.
- ✅ **H4** — TUI deps gated behind the optional `tui` feature; the `runtime-host`
  profile builds with no TUI dependencies.
- ✅ **H5** (completed by Epic 11 M2/M3) — `nanosb supervise --foreground
  --name <n> --spec <file|->` spawns the supervisor as an **ownable, reapable**
  child of the caller, performs the readiness handshake, writes
  `<sandbox_dir>/supervisor.pid`, and stages the exec-agent + vsock wiring. See
  §3.1.
- ✅ **H7** — CI (`host-layer` job) asserts `nanosb-host` and the slim helper build
  without the engine or TUI dependencies.
- ✅ **Epic 178** — the guest exec agent runs **resident alongside the workload**
  (`KRUN_EXEC_AGENT`); `exec` / `exec_pty` are advertised only when the channel is
  configured and the host client retries while the agent binds. See §3.3.

A host-side consumer (the shim) can depend on **`nanosb-protocol` +
`nanosb-host` only** — for spec→config conversion, exec-agent staging, and
supervisor control — with no TUI and no libkrun.

## 5. Where the source of truth lives

| Concern | Source |
|---------|--------|
| Wire protocol | `docs/protocol/protocol.md`, `crates/protocol` |
| Capabilities / handshake | `crates/protocol/src/hello.rs` |
| Config types (FFI-free) | `runtime/crates/runtime-config/src/lib.rs` |
| Host layer (client/state/spec/deploy) | `crates/host/src/` |
