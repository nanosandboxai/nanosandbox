# Nanosandbox

A lightweight, VM-based sandbox SDK for secure code execution using [libkrun](https://github.com/containers/libkrun) and [crun](https://github.com/containers/crun).

## Overview

Nanosandbox provides hardware-isolated execution environments with near-container performance. Unlike traditional containers that share the host kernel, Nanosandbox runs each sandbox in its own microVM, providing stronger security guarantees.

## Key Features

- **VM-Level Isolation**: Each sandbox runs in its own microVM using KVM (Linux) or HVF (macOS)
- **OCI Image Support**: Use any container image from Docker Hub, GHCR, or private registries
- **Fast Boot Times**: Sub-second VM startup using libkrun's optimized VMM
- **Transparent Networking**: TSI (Transparent Socket Impersonation) for seamless network access
- **Registry Authentication**: Support for private registries via Docker config.json
- **Cross-Platform**: Supports Linux (KVM), macOS Apple Silicon (HVF), and Windows (Containers)

## Platform Support

| Platform | Runtime | Hypervisor | Container Images |
|----------|---------|------------|------------------|
| **Linux** | crun/krun | KVM | Linux images (Alpine, Ubuntu, etc.) |
| **macOS** | krunvm | HVF (Hypervisor.framework) | Linux images (Alpine, Ubuntu, etc.) |
| **Windows** | Windows Containers | HCS / Hyper-V | **Windows images only** |

### Windows Limitation

> **Important**: Windows containers can only run **Windows container images** (e.g., `nanoserver`, `servercore`). Linux images like Alpine or Ubuntu are **not supported** on Windows.
>
> For Linux container workloads, use Linux or macOS.

**Windows container images:**
- `mcr.microsoft.com/windows/nanoserver:ltsc2022` (lightweight)
- `mcr.microsoft.com/windows/servercore:ltsc2022` (full)

## Installation

### Prerequisites

#### Linux
- Rust 1.70+ (for building from source)
- [crun](https://github.com/containers/crun) with libkrun support, or [krun](https://github.com/containers/libkrun)
- KVM enabled (`/dev/kvm` accessible)

#### macOS (Apple Silicon)
- Rust 1.70+ (for building from source)
- [krunvm](https://github.com/containers/krunvm): `brew tap slp/krun && brew install krunvm`
- macOS 11+ on Apple Silicon (M1/M2/M3/M4)

#### Windows
- Rust 1.70+ (for building from source)
- Windows 10/11 Pro, Enterprise, or Windows Server
- Containers feature enabled: `Enable-WindowsOptionalFeature -Online -FeatureName Containers -All`
- (Optional) Hyper-V for Hyper-V isolation: `Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -All`

### From Source

```bash
# Clone the repository
git clone https://github.com/devdone-labs/dd-nanosandbox
cd dd-nanosandbox

# Build the CLI
cargo build --release --features cli

# Install globally (optional)
cargo install --path . --features cli
```

### Verify Installation

```bash
nanosb --version
nanosb --help
```

## Quick Start

### CLI Usage (Linux/macOS)

```bash
# Pull an image
nanosb pull alpine:3.19

# Run a command in a new sandbox
nanosb run alpine echo "Hello from sandbox!"

# Run Python code
nanosb run python:3.12 python -c "print('Hello, World!')"

# List cached images
nanosb images

# List running sandboxes
nanosb ps

# Stop a sandbox
nanosb stop <sandbox-id>

# Remove a sandbox
nanosb rm <sandbox-id>
```

### CLI Usage (Windows)

```powershell
# Pull a Windows container image
nanosb.exe pull mcr.microsoft.com/windows/nanoserver:ltsc2022

# Run a command in a Windows container
nanosb.exe run mcr.microsoft.com/windows/nanoserver:ltsc2022 cmd.exe /c "echo Hello from Windows sandbox!"

# List cached images
nanosb.exe images

# List running sandboxes
nanosb.exe ps
```

### SDK Usage

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

## CLI Commands

| Command | Description |
|---------|-------------|
| `nanosb pull <image>` | Pull an image from a registry |
| `nanosb images` | List cached images |
| `nanosb run <image> [cmd]` | Run command in new sandbox |
| `nanosb exec <id> <cmd>` | Execute in running sandbox |
| `nanosb ps` | List sandboxes |
| `nanosb stop <id>` | Stop a sandbox |
| `nanosb rm <id>` | Remove a sandbox |

For detailed CLI documentation, see [docs/CLI.md](docs/CLI.md).

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                      Application                                 │
│              (DD-Code, CLI tools, etc.)                         │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                    Nanosandbox SDK                               │
│      (Rust crate with async API for sandbox management)         │
└─────────────────────────────────────────────────────────────────┘
                              │
              ┌───────────────┴───────────────┐
              ▼                               ▼
┌─────────────────────────┐     ┌─────────────────────────┐
│    Image Manager        │     │   Sandbox Manager       │
│  (OCI pull/push/cache)  │     │ (Lifecycle, exec, I/O)  │
└─────────────────────────┘     └─────────────────────────┘
              │                               │
              └───────────────┬───────────────┘
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                   Runtime Layer                                  │
│            (crun with libkrun handler)                          │
└─────────────────────────────────────────────────────────────────┘
                              │
              ┌───────────────┴───────────────┐
              ▼                               ▼
┌─────────────────────────┐     ┌─────────────────────────┐
│       libkrun           │     │      libkrunfw          │
│   (VMM + virtio)        │     │   (Guest firmware)      │
└─────────────────────────┘     └─────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                 Hardware Virtualization                          │
│      KVM (Linux) / HVF (macOS) / HCS (Windows)                  │
└─────────────────────────────────────────────────────────────────┘
```

## Components

### nanosandbox (SDK)

The main Rust crate providing:

- `Sandbox` - High-level sandbox management
- `ImageManager` - OCI image operations with authentication
- `Runtime` - Low-level crun/libkrun interface
- `SandboxRegistry` - Sandbox state persistence

### nanosb (CLI)

Command-line interface for:

- Image management (`nanosb pull`, `nanosb images`)
- Sandbox operations (`nanosb run`, `nanosb exec`, `nanosb stop`, `nanosb rm`)
- JSON output for scripting (`--format json`)

## Comparison with Alternatives

| Feature | Nanosandbox | Microsandbox | Docker | gVisor |
|---------|-------------|--------------|--------|--------|
| Isolation | VM (KVM/HVF/HCS) | VM (libkrun) | Namespace | User-space kernel |
| OCI Registry Support | Any | Own registry | Any | Any |
| Linux Support | Yes (KVM) | Yes | Yes | Yes |
| macOS Support | Apple Silicon | Apple Silicon | Yes | No |
| Windows Support | Windows containers* | No | Yes | No |
| Boot Time | <1s | <1s | <0.5s | <0.5s |
| Self-Hosted | Yes | Requires server | Yes | Yes |

*Windows support is limited to Windows container images only (nanoserver, servercore). Linux images require Linux or macOS.

## Status

**Stage: Active Development**

- [x] M1: Foundation (OCI image pulling, layer caching, crun integration)
- [x] M2: Sandbox Management (lifecycle, exec, streaming, timeouts)
- [x] M3: Advanced Features (auth, virtio-fs, TSI networking)
- [x] M4: Production Ready (CLI, testing, documentation)
- [ ] M5: Extended Features (HTTP API, GPU passthrough, checkpoint/restore)

## Documentation

- [CLI Reference](docs/CLI.md) - Command-line interface documentation
- [Design Document](docs/DESIGN.md) - Technical design and architecture

## License

Apache-2.0

## Related Projects

- [libkrun](https://github.com/containers/libkrun) - VM-based isolation library
- [crun](https://github.com/containers/crun) - Fast OCI container runtime
- [DD-Code](https://github.com/devdone-labs/dd-code) - IDE that uses Nanosandbox
