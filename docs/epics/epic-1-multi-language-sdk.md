# Epic 1 — Multi-language Sandbox SDK (protocol-first, helper-based)

Status: decisions locked (2026-10-07) — see §0
Date: 2026-10-07
Related: `docs/epics/epic-2-tui-refactor-and-testing.md`, PR #91

## 0. Decisions (locked 2026-10-07)

| # | Decision |
|---|---|
| D1 | **Substrate = a single slim helper** (`nanosb-runtime`). All SDKs are **protocol clients** over its local socket. |
| D2 | **No C FFI in the SDK.** The C ABI is retired (WS8): not the substrate, not shipped as an SDK. |
| D3 | **Bundle only** (macOS arm64): helper + `libkrunfw` + `gvproxy` ship inside each package. External override (`NANOSB_HOME`) remains for Linux/CI. |
| D4 | **Slim helper via feature flags** (one crate, two profiles). |
| D5 | **Pre-sign at package build time**; no install-time signing; lazy re-sign fallback. |
| D6 | **Cloud later**: `SandboxClient` backend abstraction now; implement only `Local`. |
| D7 | **Protocol framing = NDJSON** (matches today's control socket). |
| D8 | **Full snapshots** (writable layer + manifest; restore + fork) **and pause/resume**, in scope for this epic. |
| D9 | **guest `fs` API in scope**; v1 verb set fixed in §4.3. |
| D10 | **Networking engine → Epic 3** (depends on this epic). This epic ships only the networking **config surface**: published ports (TCP/UDP/bind) + policy-config passthrough over the protocol. |
| D11 | **Owned volumes deferred** — an owned volume is the sandbox writable layer, already covered by snapshots; adds surface without new capability. |

## 1. Context (verified 2026-10-07)

**Why the helper, not FFI.** The VM cannot be hosted in-process: `hv_vm_create()`
on macOS needs the `com.apple.security.hypervisor` entitlement in a clean
single-threaded process. The runtime already boots the VM in a subprocess
(`internal-boot-vm`) spawned from a codesigned binary. So an "in-process FFI"
still shells out — FFI buys no in-process benefit, adds a hard ABI contract,
makes streaming callbacks bespoke per language, and would need a native addon +
codesign per language × platform.

**Runtime artifacts.**

| Artifact | Linkage | Location today |
|---|---|---|
| libkrun | static, linked at build time | compiled into the helper |
| `libkrunfw.5.dylib` | `dlopen` at runtime | `~/.nanosandbox/libs/` or system |
| `gvproxy` | sidecar process | `which gvproxy` or `~/.nanosandbox/bin/` |
| VM host | subprocess (`internal-boot-vm`) | the helper binary |

**microsandbox (the bar):** 5 languages (TS, Rust, Python, Go, Ruby); a rich
per-sandbox surface (§4); `local | cloud` backends; an "Agent Client" protocol.

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
                                                     ├── console / logs / metrics
                                                     ├── control socket (attach/exec/fs/logs)
                                                     └── deploy planner (mounts, agent cmd)
                                                     + libkrunfw + gvproxy
```

- **All planning stays in the helper.** Clients send intent and receive results;
  they never reimplement mount planning, agent command, or secret resolution.
- **One transport for everything.** `exec_stream`, `attach`, `logs --follow`,
  `fs` streams all ride the same socket.
- The agent's `fs`/exec verbs use the **vsock exec channel**, so they work with
  networking disabled — exactly microsandbox's design ("same channel as command
  execution, not the network").

## 3. Goal

Ship **idiomatic SDKs in more languages than microsandbox**, on one stable,
helper-based protocol, with a sandbox surface that **matches or exceeds**
microsandbox's — no native addons, no C ABI, no per-language signing.

## 4. Missing-interface inventory (vs microsandbox)

Legend: **[A]** SDK-surface work · **[R]** runtime work · **[D]** deliberate divergence.

### 4.1 Lifecycle & handles
| Interface | nanosandbox | Target |
|---|---|---|
| create / start / stop / kill / remove | ✅ (CLI/supervisor) | expose via protocol |
| `get`, `list`, `listWith` (filters, pagination) | partial (`ps`) | **[A]** |
| handles (lightweight, read-only vs live) | ❌ | **[A]** |
| detached mode | ✅ | expose |
| `ephemeral` | ❌ | **[A]** |
| `replace` / `replaceWithTimeoutMs` | ❌ | **[A]** |
| `waitUntilStopped` | poll | **[A]** |
| `ping` / `touch` (keepalive) | ❌ | **[A]** |
| `requestDrain` / `requestStop` / `requestKill` | partial | **[A]** |
| `idleTimeoutSecs` / `maxDurationSecs` | timeout only | **[A]** |
| `pause` / `resume` | ❌ | **[R]** (M5) |
| `snapshot` / `restore` / `fork` | ❌ | **[R]** (M5) |

### 4.2 Execution
| Interface | nanosandbox | Target |
|---|---|---|
| `exec` / `execWith` (cwd, env, user, timeout) | ✅ | parity |
| `exec_stream` | ✅ | parity |
| `shell` / `shell_stream` | ✅ | parity |
| `attach` (interactive PTY) | ✅ (`--tty`) | full detach/resize |
| `stdin` (null / pipe / bytes) | pipe | add null/bytes modes |
| per-exec `rlimit` | ❌ | **[A]** |
| default-workload variants (`exec_default`, `attach_default`) | ❌ | **[A]** |
| `tty` | ✅ | parity |

### 4.3 Filesystem
**v1 set (ship):** `read`, `readToString`, `readStream`, `write`, `writeStream`,
`list`, `stat`, `exists`, `mkdir`, `remove`, `removeDir`, `copy`, `rename`,
`copyFromHost`, `copyToHost`.

| Interface | nanosandbox | Target |
|---|---|---|
| `fs.read` / `readToString` / `readStream` | ❌ | **[A] v1** |
| `fs.write` / `writeStream` | ❌ (upload only) | **[A] v1** |
| `fs.list` / `stat` / `exists` | ❌ | **[A] v1** |
| `fs.mkdir` / `remove` / `removeDir` | ❌ | **[A] v1** |
| `fs.copy` / `rename` | ❌ | **[A] v1** |
| `fs.copyFromHost` / `copyToHost` | upload-only | **[A] v1** |
| `fs.symlink` / `readLink` / `realPath` | ❌ | **[A] defer** |
| `fs.open` handles / `fstat` / `setStat` | ❌ | **[A] defer** |

> Transport: exec-agent file verbs over vsock (works without a project mount).
> All ops run as the configured guest user, inheriting that containment; no
> additional path scope is imposed (matches microsandbox).

### 4.4 Observability
| Interface | nanosandbox | Target |
|---|---|---|
| `metrics` (CPU / mem / disk / net) | ❌ | **[A]** |
| `metricsStream` | ❌ | **[A]** |
| `logs` (structured) | raw `console.log` | **[A]** |
| `logStream` (follow + cursor) | ❌ | **[A]** |
| log sources (stdout / stderr / pty / system) | ❌ | **[A]** |
| `labels` (metric attribution) | ❌ | **[A]** |

### 4.5 Secrets
| Interface | nanosandbox | Target |
|---|---|---|
| `secret` / `secretEnv` (named entries) | ❌ (env passthrough) | **[A]** |
| secret store / substitution | ❌ | **[A]** (basic) |
| `onSecretViolation` (network substitution) | ❌ | **[R]** (with egress) |
| never persist secret values | ✅ | keep invariant |

### 4.6 Storage / volumes
| Interface | nanosandbox | Target |
|---|---|---|
| bind mount (dir) | ✅ (project/virtiofs) | expose |
| file mount | ❌ | **[A]** |
| named volumes (dir-backed / disk-backed) | ❌ | **[A]** |
| disk-image volumes (`raw`/`qcow2`/`vmdk` + fstype) | ❌ | **[A]** |
| tmpfs | ❌ | **[A]** |
| owned volumes | ❌ | **defer** (D11 — equals the writable layer) |
| mount options (`noexec`/`nosuid`/`nodev`) | ❌ | **[A]** |
| mount owner (`uid`/`gid`) | ❌ | **[A]** |
| stat virtualization (`strict`/`relaxed`/`off`) | partial (image xattrs) | **[R]** |
| `quota` | ❌ | **[R]** |
| nested mount destinations | ❌ | **[A]** |
| `Volume.get/list` + `volume.fs()` (host-side) | ❌ | **[A]** |

### 4.7 Networking
This epic ships the **config surface**; the **enforcement engine is Epic 3**
(depends on this epic). Legend: **[A]** config surface here · **[E3]** Epic 3.

| Interface | nanosandbox | Target |
|---|---|---|
| publish TCP port (loopback default) | ✅ (`--port`) | **[A]** parity |
| publish with explicit bind address | ❌ | **[A]** |
| publish UDP port | ❌ | **[A]** |
| policy-config passthrough (`NetworkConfig`) | ❌ | **[A]** |
| network policy enforcement (deny-by-default, profiles) | ❌ | **[E3]** |
| rule allowlists (IP / CIDR / domain / domain-suffix / port-range) | ❌ | **[E3]** |
| DNS interception / filtering | ❌ | **[E3]** |
| TLS interception | ❌ | **[E3]** |
| rate limiting / max connections | ❌ | **[E3]** |
| interface overrides (ipv4/ipv6 pools, mac, mtu) | ❌ | **[E3]** |
| `trustHostCAs` | ❌ | **[E3]** |
| host access (`host.*.internal`) | ❌ | **[E3]** |
| NAT64 prefixes | ❌ | **[E3]** |

### 4.8 Config / images
| Interface | nanosandbox | Target |
|---|---|---|
| OCI image source | ✅ | parity |
| local directory rootfs | ❌ | **[R]** |
| disk-image rootfs (+ fstype) | ❌ | **[R]** |
| `pullPolicy` (always / …) | ❌ (always pulls) | **[A]** |
| registry config (per-sandbox) | partial | **[A]** |
| pre-boot rootfs patches (copyFile/copyDir/text/mkdir/remove) | ❌ | **[R]** |
| named scripts (`/.msb/scripts/`) | ❌ | **[A]** |
| `entrypoint` / `cmd` / `hostname` overrides | ✅ (command) | parity |
| `securityProfile` (`default`/`restricted`) | ❌ | **[R]** |
| `max_cpus` / `max_memory` (hotplug ceilings) | ❌ | **[R]** (defer) |
| `guest_clock` / THP policy | ❌ | **[R]** (defer) |
| `libkrunfwPath` / runtime path override | `NANOSB_HOME` | parity |

### 4.9 Divergence
| Interface | microsandbox | nanosandbox | Note |
|---|---|---|---|
| `ssh()` | ✅ | removed | **[D]** — expose `exec`/`attach` instead; document why (SSH was a network-reachable surface we deliberately dropped) |
| `attach` via SSH | ✅ | console/vsock | **[D]** — arguably more secure (no network listener) |

## 5. Workstreams

### WS1 — `nanosb-runtime` helper (D4)
Feature `runtime-host`; build with `--no-default-features --features runtime-host`;
exclude TUI/`ratatui`/`arboard`/`rusqlite`/`git2`.
AC: helper boots + controls a sandbox; materially smaller than the CLI.

### WS2 — Local protocol (D7)
Versioned NDJSON verbs: `hello`, `create/start/stop/kill/remove`, `status`,
`list/get`, `exec`, `exec_stream`, `attach`, `logs` (follow+cursor), `metrics`,
`fs/*`, `volumes/*`, `secrets/*`, `snapshot/restore`, `pause/resume`, `drain`,
`ping/touch`, `modify`.
AC: spec in `docs/`; reference client exercises every verb.

### WS3 — `SandboxClient` + backend abstraction (D6)
Contract: builder, create, get, list, remove, start, stop, kill, pause, resume,
snapshot, restore, exec, exec_stream, shell, attach, logs, metrics, fs, volumes,
secrets, config, detached, drain, ping. `Local` implemented; `Cloud` stubbed.

### WS4 — Core surface expansion (in the helper)
`fs` (verbs over vsock), `metrics`, structured `logs`, `secrets`, `rlimits`,
`volumes` (+ options), `handles`, `labels`, `ephemeral`, `pullPolicy`, scripts.

### WS5 — Networking config surface (engine is Epic 3)
- Expose published ports (TCP/UDP, explicit bind address, loopback default) via
  the protocol.
- Pass a `NetworkConfig` / policy object through to the runtime (the helper
  serializes it into the sandbox config).
- The **enforcement engine** (policy, allowlists, DNS, TLS, rate limits,
  interface overrides, host CA trust, host access, NAT64, strict mode) is
  **Epic 3 — Runtime Networking & Isolation**, which depends on this epic's
  helper + protocol.
- AC: `SandboxBuilder.network(...)` config round-trips to the helper; published
  TCP/UDP ports work end-to-end.

### WS6 — Snapshots / pause-resume **[R]** (D8 — full)
- **Full snapshot**: writable layer + manifest pinning the base image, with an
  optional integrity hash; `restore` yields a new independent sandbox; `fork`
  branches from a snapshot.
- `pause` / `resume` (execution state).
- AC: snapshot a running sandbox → restore into a new one; pause stops execution
  and resume continues.

### WS7 — Language SDKs (protocol clients only)
Rust, Python, TS/Node, Go, Ruby, **Java/Kotlin**, **C#/.NET**. Each passes the
conformance suite.

### WS8 — Retire the C FFI (D2)
Delete `ffi.rs`, the `ffi` feature, cbindgen; update `sandbox/Cargo.toml`.
AC: `grep -r 'extern "C" fn sandbox_' sandbox/` empty.

### WS9 — Packaging & signing (D3, D5)
Bundle helper + `libkrunfw` + `gvproxy` (macOS arm64) in each package; pre-sign
at build; lazy re-sign fallback; `NANOSB_HOME` override; "Runtime setup" doc.

### WS10 — Conformance suite & docs
One spec per language: lifecycle, exec/stream/shell/attach, fs, logs, metrics,
volumes, secrets, snapshots, errors, timeouts.

## 6. Acceptance criteria

- AC1 `nanosb-runtime` builds from features; boots + controls a sandbox.
- AC2 Protocol spec published; reference client passes every verb.
- AC3 Rust/Python/TS/Go/Ruby SDKs pass conformance.
- AC4 Java/Kotlin + C#/.NET pass conformance (exceeds microsandbox).
- AC5 exec/exec_stream/shell/attach/fs/logs/metrics/volumes work in every binding
  on a next-mode vanilla image.
- AC6 Snapshot + restore and pause + resume work (local).
- AC7 No binding persists secret values.
- AC8 The C FFI is fully removed; no native addon dependency.
- AC9 Bundled macOS arm64 install runs with zero extra setup; `NANOSB_HOME` works.

## 7. Milestones

- **M0** Helper build profile + protocol spec + reference client (WS1, WS2).
- **M1** `SandboxClient` + `Local`; retire the C FFI (WS3, WS8).
- **M2** Core surface: fs/metrics/logs/secrets/volumes/handles (WS4).
- **M3** Python + TypeScript SDKs (WS7).
- **M4** Go + Ruby SDKs (WS7) — matches microsandbox's language set.
- **M5** Full snapshots + pause/resume (WS6).
- **M6** Java/Kotlin + C#/.NET; packaging/signing; conformance + docs (WS7, WS9, WS10).

> The networking **engine** is **Epic 3** (`docs/epics/epic-3-runtime-networking.md`),
> which depends on this epic's helper + protocol.

## 8. Risks

- R1 **Surface size** — microsandbox's interface is vast; stage it (M2 core, M5
  advanced). Version the protocol; don't ship a half-implemented verb set.
- R2 **Networking engine is deferred to Epic 3** (largest runtime effort) — this
  epic ships the config surface + ports only; Epic 3 owns enforcement.
- R3 **Full snapshots on libkrun** (writable-layer capture + manifest) → spike
  early in M5; if writable-layer capture proves disproportionate, fall back to
  disk-only snapshots and mark memory/process state out of scope.
- R4 **`fs` transport** → exec-agent verbs (chosen); validate throughput vs
  microsandbox's warning that bulk transfers should use volumes.
- R5 **7-language packaging** → automation; macOS arm64 first; `NANOSB_HOME`
  covers Linux/CI.
- R6 **Divergence from `ssh()`** — document; provide exec/attach as the answer.

## 9. Resolved decisions

- **Q1 Networking: engine moved to Epic 3** (D10); this epic ships the config
  surface (ports + policy passthrough).
- **Q2 Snapshots: FULL** (writable layer + manifest; restore/fork) (D8).
- **Q3 `fs` v1 verb set:** fixed in §4.3.
- **Q4 Owned volumes: deferred** (D11).
