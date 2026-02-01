# Nanosandbox

A lightweight, VM-based sandbox SDK for secure code execution using [libkrun](https://github.com/containers/libkrun) and [crun](https://github.com/containers/crun).

## Overview

Nanosandbox provides hardware-isolated execution environments with near-container performance. Unlike traditional containers that share the host kernel, Nanosandbox runs each sandbox in its own microVM, providing stronger security guarantees.

## Key Features

- **VM-Level Isolation**: Each sandbox runs in its own microVM using KVM (Linux) or HVF (macOS)
- **OCI Image Support**: Use any container image from Docker Hub, GHCR, or private registries
- **Fast Boot Times**: Sub-second VM startup using libkrun's optimized VMM
- **Transparent Networking**: TSI (Transparent Socket Impersonation) for seamless network access
- **GPU Passthrough**: Support for GPU acceleration via virtio-gpu
- **Cross-Platform**: Supports Linux (KVM) and macOS Apple Silicon (HVF)

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
│             KVM (Linux) / HVF (macOS ARM64)                     │
└─────────────────────────────────────────────────────────────────┘
```

## Components

### nanosandbox (SDK)

The main Rust crate providing:

- `Sandbox` - High-level sandbox management
- `ImageManager` - OCI image operations
- `Runtime` - Low-level crun/libkrun interface

### nanosandbox-cli

Command-line interface for:

- Image management (`nano pull`, `nano images`)
- Sandbox operations (`nano run`, `nano exec`, `nano stop`)
- Server mode for API access

### nanosandbox-server (optional)

HTTP/gRPC server for remote sandbox orchestration.

## Comparison with Alternatives

| Feature | Nanosandbox | Microsandbox | Docker | gVisor |
|---------|-------------|--------------|--------|--------|
| Isolation | VM (KVM/HVF) | VM (libkrun) | Namespace | User-space kernel |
| OCI Registry Support | Any | Own registry | Any | Any |
| macOS Support | Apple Silicon | Apple Silicon | Yes | No |
| GPU Passthrough | Yes | Yes | Yes | Limited |
| Boot Time | <1s | <1s | <0.5s | <0.5s |
| Self-Hosted | Yes | Requires server | Yes | Yes |

## Status

**Stage: Design & Analysis**

This project is currently in the design phase. See the [design document](docs/DESIGN.md) for detailed specifications.

## License

Apache-2.0

## Related Projects

- [libkrun](https://github.com/containers/libkrun) - VM-based isolation library
- [crun](https://github.com/containers/crun) - Fast OCI container runtime
- [DD-Code](https://github.com/devdone-labs/dd-code) - IDE that will use Nanosandbox
