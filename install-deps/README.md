# nanosandbox install-deps

Runtime dependency packaging and installation for [nanosandbox](https://github.com/nanosandboxai/runtime).

## What gets installed

| Component | Description | Install path |
|-----------|-------------|-------------|
| **libkrunfw** | Kernel firmware loaded at VM boot | `/usr/local/lib/` |
| **gvproxy** | User-mode networking daemon | `~/.local/bin/` |

## Install

```bash
curl -fsSL https://github.com/nanosandboxai/install-deps/releases/latest/download/install.sh | bash
```

### Options

```bash
# Install a specific version
DEPS_VERSION=v0.2.0 bash install.sh

# Custom install paths
LIB_DIR=/opt/nanosandbox/lib BIN_DIR=/opt/nanosandbox/bin bash install.sh
```

## Uninstall

```bash
curl -fsSL https://github.com/nanosandboxai/install-deps/releases/latest/download/uninstall.sh | bash
```

## How it works

1. **runtime** repo builds libkrunfw + gvproxy for all platforms during release CI
2. Runtime CI triggers `repository_dispatch` to this repo
3. This repo packages the deps with install scripts and creates a GitHub Release
4. Users (or the CLI installer) download and run `install.sh`

## Platform support

| Platform | Architecture | Bundle |
|----------|-------------|--------|
| Linux | x86_64 | `deps-linux-amd64.tar.gz` |
| Linux | aarch64 | `deps-linux-arm64.tar.gz` |
| macOS | Apple Silicon | `deps-darwin-arm64.tar.gz` |

## Prerequisites

- **macOS**: Apple Silicon with Hypervisor.framework
- **Linux**: KVM support (`/dev/kvm` accessible)
