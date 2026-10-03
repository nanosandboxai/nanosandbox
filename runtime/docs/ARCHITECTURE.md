# Nanosandbox Runtime Architecture

## Three-Layer Virtualization Stack

```
┌─────────────────────────────────────────────────────┐
│  Layer 3: OCI Container Image (guest rootfs)        │
│  Currently: node:22-slim (Debian)                   │
│  Mounted via virtiofs — this is what runs inside VM │
└──────────────────────┬──────────────────────────────┘
                       │ virtiofs mount
┌──────────────────────┴──────────────────────────────┐
│  Layer 2: libkrun (VMM — Apache-2.0)                │
│  Creates VM, configures CPU/RAM/network/rootfs      │
│  Includes custom init.c (PID 1 inside guest)        │
│  Loaded dynamically or statically linked            │
└──────────────────────┬──────────────────────────────┘
                       │ dlopen at runtime
┌──────────────────────┴──────────────────────────────┐
│  Layer 1: libkrunfw (kernel library — LGPL-2.1)     │
│  Bundles: Linux kernel 6.12.76 (vanilla upstream)   │
│  + 26 custom patches (TSI, vsock, ARM64 TSO, etc.)  │
│  NO distribution. NO userland. Just the kernel.     │
└─────────────────────────────────────────────────────┘
```

### Key insight

**libkrunfw is NOT a Linux distribution.** It bundles only a Linux kernel. The "distribution" that runs inside the VM comes entirely from the OCI container image (Layer 3).

## Boot Chain

```
1. Host calls krun_start_enter()
2. libkrun loads kernel from libkrunfw (dlopen + section mapping)
3. KVM/HVF creates VM, injects kernel into guest memory
4. Kernel boots (< 100ms — monolithic, no modules)
5. Kernel runs init binary as PID 1 (from libkrun, not libkrunfw)
6. init mounts /dev, /proc, /sys, cgroups
7. init reads /.krun_config.json
8. init does chroot() to rootfs (virtiofs-mounted OCI image layers)
9. init forks child → child execs user's command (nanosb-init.sh)
10. nanosb-init.sh configures networking, starts sshd, execs agent-gateway
```

## Monorepo Structure

```
runtime/
├── crates/
│   ├── nanosandbox/        Rust SDK — VM sandbox management, OCI image pulling
│   └── libkrun-sys/        Rust FFI bindings for libkrun C API
├── deps/
│   ├── libkrun/            Git submodule → containers/libkrun (Apache-2.0)
│   ├── libkrunfw/          Git submodule → containers/libkrunfw (LGPL-2.1)
│   └── gvproxy/            Git submodule → containers/gvisor-tap-vsock (Apache-2.0)
├── agent-gateway/          Go HTTP server — runs as PID 1 inside VMs
├── docker/                 Container images for agent VMs (Debian slim)
├── scripts/                Build and CI helper scripts
└── .github/workflows/      CI: lint, multi-distro build, release
```

### Component relationships

- **nanosandbox** depends on **libkrun-sys** for FFI bindings
- **libkrun-sys** finds libkrun via: submodule build → pkg-config → known paths → env var
- **libkrun** (submodule) depends on **libkrunfw** at runtime (dlopen)
- **gvproxy** provides user-mode networking (virtio-net); optional, TSI fallback when absent
- **agent-gateway** is independent (Go binary, built separately)
- **Docker images** bundle agent-gateway + agent CLIs on Debian slim

## Building

### Full build (all components)
```bash
git submodule update --init --recursive
./scripts/build-all.sh
```

### Individual components
```bash
./scripts/build-all.sh libkrunfw    # kernel library
./scripts/build-all.sh libkrun      # VMM
./scripts/build-all.sh gvproxy      # networking sidecar
./scripts/build-all.sh nanosandbox  # Rust SDK
./scripts/build-all.sh gateway      # Go server
```

### Just the Rust workspace (uses system libkrun)
```bash
cargo build -p nanosandbox
```

## Cross-Distro Support

### Host requirements (where nanosandbox runs)

| Platform | Hypervisor | Status |
|----------|-----------|--------|
| macOS Apple Silicon | HVF | Stable |
| Linux x86_64 | KVM | In Development |
| Linux aarch64 | KVM | In Development |
| Windows x86_64 | HCS | In Development |

### Windows HCS backend

The Windows port uses the **Host Compute Service (HCS)** — Microsoft's native
hypervisor API — instead of KVM or HVF. Key differences from Unix backends:

- **hcs crate** (`deps/libkrun/src/hcs/`): Rust wrapper around the HCS C API
  (`computecore.dll`, `computenetwork.dll`). Creates and manages Hyper-V
  isolated containers as lightweight VMs.
- **libkrunfw-win** (`deps/libkrun/src/libkrunfw-win/`): Embeds the Linux
  kernel binary (`vmlinux.bin`) at compile time via `include_bytes!`. The
  kernel is NOT checked into git — it is built by the `build-kernel.yml` CI
  workflow or downloaded manually.
- **vm-memory-win** (`deps/libkrun/src/vm-memory-win/`): Vendored fork of
  `vm-memory` with Windows memory-mapping support. Activated via
  `[patch.crates-io]` in the workspace `Cargo.toml`.
- **Static rlib linking**: On all three platforms (Linux, macOS, Windows),
  `libkrun-sys` links libkrun as a static Rust rlib. On Windows there is no
  dlopen of libkrunfw — the kernel is embedded directly.

### CI matrix (validated distros)

| Distribution | Package Manager | Status |
|-------------|----------------|--------|
| Ubuntu 24.04 | apt-get | CI validated |
| Debian 12 | apt-get | CI validated |
| Fedora 40 | dnf | CI validated |
| Arch Linux | pacman | CI validated |
| Alpine 3.20 | apk (musl) | CI validated |
| openSUSE Tumbleweed | zypper | CI validated |

### Custom install paths

```bash
# Override libkrun location
export NANOSANDBOX_LIBKRUN_PATH=/custom/path/libkrun.so

# Override libkrunfw location
export NANOSANDBOX_LIBKRUNFW_PATH=/custom/path/libkrunfw.so
```

## Licensing

| Component | License | Linking |
|-----------|---------|---------|
| nanosandbox | Apache-2.0 | — |
| libkrun | Apache-2.0 | Static or dynamic |
| libkrunfw | LGPL-2.1 | Always dynamic (dlopen by libkrun) |
| Linux kernel (inside libkrunfw) | GPL-2.0 | Embedded in libkrunfw.so |
| gvproxy (gvisor-tap-vsock) | Apache-2.0 | Standalone binary |
| agent-gateway | Apache-2.0 | — |

Static linking of libkrun into nanosandbox is safe — libkrunfw (LGPL/GPL) is always loaded dynamically at runtime by libkrun on Linux/macOS. On Windows, the kernel is embedded via libkrunfw-win (no dlopen).

## Release Pipeline

```
runtime repo (CI)
    ↓ build on tag push
    ↓ matrix: macOS arm64, Linux amd64, Linux arm64
    ↓
GitHub Release (tarballs + checksums)
    ↓
installations repo (install.sh)
    ↓ curl | bash
End user
```

### Release artifacts per platform

| Artifact | Contents |
|----------|----------|
| `deps-linux-amd64.tar.gz` | libkrunfw.so, libkrun.so, gvproxy |
| `deps-darwin-arm64.tar.gz` | libkrunfw.dylib, libkrun.dylib, gvproxy |
| `nanosandbox-{os}-{arch}.tar.gz` | nanosandbox binary |
| `agent-gateway-linux-{arch}.tar.gz` | agent-gateway binary |
| `deps-windows-amd64.tar.gz` | nanosandbox.exe (kernel embedded) |
