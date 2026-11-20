# Nanosandbox Runtime

VM-based sandbox engine using libkrun FFI.

> **Platform Status**: **macOS Apple Silicon** is stable. **Linux x86_64 (KVM)** is
> live-verified (see [`../docs/live-verification-linux.md`](../docs/live-verification-linux.md));
> Linux aarch64 uses the same code path but is not yet exercised. Windows support
> has been archived (see the `archive/windows-track` branch).

## Overview

Nanosandbox Runtime is a VM engine providing hardware-isolated execution
environments. Each sandbox runs in its own microVM, delivering stronger security
guarantees than traditional containers that share the host kernel.

This crate handles VM lifecycle, OCI image management, and containerization. It
does **not** contain agent logic — agent-specific functionality lives in the
in-repo [Sandbox SDK](../sandbox).

## Key Features

- **VM-Level Isolation** — Each sandbox runs in its own microVM via libkrun (KVM/HVF)
- **OCI Image Support** — Use any container image from Docker Hub, GHCR, or private registries
- **Sub-Second Boot Times** — Optimized VM startup using libkrun
- **TSI Networking** — Transparent Socket Impersonation for seamless network access
- **Next-Mode Console Boot** — console I/O fds, extra virtiofs mounts, and microVM-layer network bring-up for vanilla images
- **Cross-Platform** — macOS Apple Silicon (stable), Linux x86_64 KVM (live-verified)

## Architecture

```
Application (CLI, SDKs)  [../src, ../sdk]
       |
   Sandbox SDK  [../sandbox]
       |
   Nanosandbox Runtime (this crate)
       |
   +-------+-------+
   |               |
   Image           Runtime Backend
   Manager         (libkrun FFI)
   |               |
   OCI             libkrun + libkrunfw
   Registry        |
                   Hardware Virtualization
                   (KVM / HVF)
```

## Public API

Key exports from the `runtime` crate:

| Export | Description |
|--------|-------------|
| `Sandbox` | Lifecycle management: create, start, stop, destroy, exec |
| `SandboxConfig` | Builder for VM configuration (CPUs, memory, image, mounts, networking) |
| `ImageManager` | OCI image operations: pull, list, remove, prune |
| `Runtime` | Low-level platform abstraction over libkrun FFI |

Additional re-exports include `SandboxConfig`, `ExecResult`, `ExecOptions`,
`SandboxStatus`, `SandboxRegistry`, `SandboxInfo`, `OciBundle`, `CredentialStore`,
`NetworkConfig`, `NetworkMode`, `NetworkScope`, `Mount`, `MountType`,
`PortMapping`, `ImageRef`, `PulledImage`, `PruneResult`. The exact set is defined
in `crates/runtime/src/lib.rs`.

## Platform Support

| Platform | Runtime | Hypervisor | Status |
|----------|---------|------------|--------|
| **macOS** | libkrun FFI | HVF (Hypervisor.framework) | **Stable** |
| **Linux** | libkrun FFI | KVM | **Live-verified (x86_64)** |

### Platform Notes

- **macOS Apple Silicon (M1/M2/M3/M4)**: Stable. Use the install script for setup.
- **Linux x86_64**: Live-verified on KVM. Requires `/dev/kvm` accessible to the user.
- **Windows**: archived (see the `archive/windows-track` branch).

## Build Instructions

### Prerequisites

- Rust (see `rust-toolchain.toml`)
- libkrunfw (guest firmware) — built from the submodule
- macOS: `brew install lld llvm` (for cross-compiling libkrun's init blob)
- Linux: KVM enabled (`/dev/kvm` accessible)

### Build libkrun (first time only)

libkrun is consumed as a prebuilt static library from the nanosandbox fork. Build it once:

```bash
./scripts/build-libkrun.sh
```

This clones [nanosandboxai/libkrun](https://github.com/nanosandboxai/libkrun)
branch `nanosandbox` (base upstream v1.19.5) at a pinned SHA, builds it as
`libkrun.a`, and places it in `~/.nanosandbox/lib/`. The fork carries the
nanosandbox customizations (next-mode init, macOS virtiofs hardening) as commits.

### Build

```bash
cargo build -p runtime
```

### Test

```bash
cargo test -p runtime --lib
```

### Custom libkrun path

```bash
export LIBKRUN_LIB_DIR=/path/to/libkrun.a/dir
cargo build -p runtime
```

## Driving the runtime

The runtime is driven in-process by the `nanosb` supervisor and out-of-process
through the `nanosb-runtime` helper over the versioned NDJSON protocol (`exec`,
`wait`, `kill`, PTY). See [`../docs/protocol/protocol.md`](../docs/protocol/protocol.md)
and the [`../sdk`](../sdk) clients.

The in-guest exec agent runs **resident alongside the PID-1 workload** (Epic 178):
when a workload command is set the runtime exports `KRUN_EXEC_AGENT` so the
libkrun fork's init backgrounds the agent, keeping the workload as PID 1. So
`nanosb exec` and the containerd shim's `exec` work for a normal container, not
only for the no-command case. See
[`../docs/runtime-capability-contract.md`](../docs/runtime-capability-contract.md) §3.3.

## This Crate Does NOT Contain

| Concern | Where to find it |
|---------|-----------------|
| Console streaming / logs | CLI ([`../src`](../src), `nanosb`) |
| MCP server management | [Sandbox SDK](../sandbox) |
| Agent configuration / `sandbox.yml` parsing | [Sandbox SDK](../sandbox) |
| Docker images for agents | [Registries](../registries) |
| Session management | [Sandbox SDK](../sandbox) |

## Related Components (monorepo)

- [Sandbox SDK](../sandbox) — Agent-aware SDK and `sandbox.yml` parsing
- [Language SDKs](../sdk) — Eight protocol clients + conformance harness
- [Registries](../registries) — Open-source MCP registry sources, agent VM images
- [CLI](../src) — `nanosb` CLI + TUI

## Comparison with Alternatives

| Feature | Nanosandbox | Microsandbox | Docker | gVisor |
|---------|-------------|--------------|--------|--------|
| Isolation | VM (KVM/HVF) | VM (libkrun) | Namespace | User-space kernel |
| OCI Registry Support | Any | Own registry | Any | Any |
| Linux Support | Yes (KVM) | Yes | Yes | Yes |
| macOS Support | Apple Silicon | Apple Silicon | Yes | No |
| Windows Support | Archived | No | Yes | No |
| Boot Time | <1s | <1s | <0.5s | <0.5s |
| Self-Hosted | Yes | Requires server | Yes | Yes |

## License

Apache-2.0
