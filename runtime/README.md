# Nanosandbox Runtime

VM-based sandbox engine using libkrun FFI.

> **Platform Status**: Currently, only **macOS Apple Silicon** is fully tested and stable.
> Linux support is in development. Windows support has been archived (see `archive/windows-track` branch).

## Overview

Nanosandbox Runtime is a pure VM engine providing hardware-isolated execution environments. Each sandbox runs in its own microVM, delivering stronger security guarantees than traditional containers that share the host kernel.

This crate handles VM lifecycle, OCI image management, and containerization. It does **not** contain agent logic -- agent-specific functionality lives in the [Sandbox SDK](https://github.com/nanosandboxai/sandbox).

## Key Features

- **VM-Level Isolation** -- Each sandbox runs in its own microVM via libkrun (KVM/HVF)
- **OCI Image Support** -- Use any container image from Docker Hub, GHCR, or private registries
- **Sub-Second Boot Times** -- Optimized VM startup using libkrun
- **TSI Networking** -- Transparent Socket Impersonation for seamless network access
- **Next-Mode Console Boot** -- console I/O fds, extra virtiofs mounts, and microVM-layer network bring-up for vanilla images
- **Cross-Platform** -- macOS Apple Silicon (stable), Linux (in development)

## Architecture

```
Application (CLI, SDKs)
       |
   Sandbox SDK (separate repo)
       |
   Nanosandbox Runtime (this repo)
       |
   +-------+-------+
   |               |
   Image           Runtime Backend
   Manager         (libkrun FFI)
   |               |
   OCI             libkrun + libkrunfw
   Registry        |
                   Hardware Virtualization
                   (KVM / HVF / WHPX)
```

## Public API

Key exports from the `nanosandbox` crate:

| Export | Description |
|--------|-------------|
| `Sandbox` | Lifecycle management: create, start, stop, destroy, exec |
| `SandboxConfig` | Builder pattern for VM configuration (CPUs, memory, image, mounts, networking) |
| `ImageManager` | OCI image operations: pull, list, remove, prune |
| `Runtime` | Low-level platform abstraction over libkrun FFI |
| `Sandbox::start_next` | Next-mode start with console fds and extra virtiofs mounts |

Additional re-exports: `SandboxConfig`, `ExecResult`, `ExecOptions`, `SandboxStatus`, `SandboxRegistry`, `SandboxInfo`, `OciBundle`, `CredentialStore`, `NetworkConfig`, `NetworkMode`, `NetworkScope`, `Mount`, `MountType`, `PortMapping`, `ImageRef`, `PulledImage`, `PruneResult`.

## Platform Support

| Platform | Runtime | Hypervisor | Status |
|----------|---------|------------|--------|
| **macOS** | libkrun FFI | HVF (Hypervisor.framework) | **Stable** |
| **Linux** | libkrun FFI | KVM | In Development |

### Platform Notes

- **macOS Apple Silicon (M1/M2/M3/M4)**: Fully tested and stable. Use the install script for easy setup.
- **Linux**: Not fully supported/tested yet. Requires libkrun installation.

## Build Instructions

### Prerequisites

- Rust 1.70+
- libkrunfw (guest firmware) — built from submodule
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
cargo build -p nanosandbox
```

### Test

```bash
cargo test -p nanosandbox
```

### Custom libkrun path

```bash
export LIBKRUN_LIB_DIR=/path/to/libkrun.a/dir
cargo build -p nanosandbox
```

## SDK Usage

```rust
use nanosandbox::{Sandbox, SandboxConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Create sandbox configuration
    let config = SandboxConfig::builder()
        .name("my-sandbox")
        .image("python:3.12-slim")
        .cpus(2)
        .memory_mb(4096)
        .build();

    // Create and start sandbox
    let mut sandbox = Sandbox::create(config).await?;
    sandbox.start().await?;

    // Execute a command
    let result = sandbox.exec("python", &["-c", "print('Hello!')"]).await?;
    println!("Output: {}", result.stdout);

    // Clean up
    sandbox.destroy().await?;
    Ok(())
}
```

## This Repo Does NOT Contain

| Concern | Where to find it |
|---------|-----------------|
| Console streaming / logs | CLI (`nanosb`) |
| MCP server management | [Sandbox SDK](https://github.com/nanosandboxai/sandbox) |
| Agent configuration / sandbox.yml parsing | [Sandbox SDK](https://github.com/nanosandboxai/sandbox) |
| Docker images for agents | [Agents Registry](https://github.com/nanosandboxai/agents-registry) |
| Session management | [Sandbox SDK](https://github.com/nanosandboxai/sandbox) |

## Related Repos

- [Sandbox SDK](https://github.com/nanosandboxai/sandbox) -- Agent-aware SDK with FFI bindings for multi-language SDKs
- [Agents Registry](https://github.com/nanosandboxai/agents-registry) -- Agent definitions, skills, Docker images
- [Nanosandbox monorepo](https://github.com/nanosandboxai/nanosandbox) -- CLI, runtime, sandbox SDK, registry, and releases

## Comparison with Alternatives

| Feature | Nanosandbox | Microsandbox | Docker | gVisor |
|---------|-------------|--------------|--------|--------|
| Isolation | VM (KVM/HVF/HCS) | VM (libkrun) | Namespace | User-space kernel |
| OCI Registry Support | Any | Own registry | Any | Any |
| Linux Support | Yes (KVM) | Yes | Yes | Yes |
| macOS Support | Apple Silicon | Apple Silicon | Yes | No |
| Windows Support | Archived | No | Yes | No |
| Boot Time | <1s | <1s | <0.5s | <0.5s |
| Self-Hosted | Yes | Requires server | Yes | Yes |

## License

Apache-2.0
