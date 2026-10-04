# nanosandbox install-deps

Runtime dependency packaging and installation for [nanosandbox](https://github.com/nanosandboxai/nanosandbox).

## What gets installed

### Linux / macOS

| Component | Description | Install path |
|-----------|-------------|-------------|
| **libkrunfw** | Kernel firmware loaded at VM boot | `/usr/local/lib/` |
| **gvproxy** | User-mode networking daemon | `~/.local/bin/` |

## Install

### Linux / macOS

```bash
curl -fsSL https://github.com/nanosandboxai/nanosandbox/releases/latest/download/install-deps.sh | bash
```

After install, open a new terminal or run `source ~/.zshrc` to pick up PATH in
your current shell.

#### Options

```bash
# Install a specific version
DEPS_VERSION=v0.2.0 curl -fsSL .../install-deps.sh | bash

# Custom install prefix
NANOSANDBOX_HOME=/opt/nanosandbox curl -fsSL .../install-deps.sh | bash
```

## Uninstall

### Linux / macOS

```bash
curl -fsSL https://github.com/nanosandboxai/nanosandbox/releases/latest/download/uninstall-deps.sh | bash
```

## How it works

1. The nanosandbox release pipeline (`release.yml`) builds libkrunfw + gvproxy (Linux/macOS).
2. The dependency bundles (`deps-*`) and these installer scripts are published together in the same GitHub Release as the CLI.
3. `install-deps.sh` downloads the bundle for the current platform from that release and installs it under `~/.nanosandbox/`.

## Platform support

| Platform | Architecture | Bundle | Script |
|----------|-------------|--------|--------|
| Linux | x86_64 | `deps-linux-amd64.tar.gz` | `install-deps.sh` |
| Linux | aarch64 | `deps-linux-arm64.tar.gz` | `install-deps.sh` |
| macOS | Apple Silicon | `deps-darwin-arm64.tar.gz` | `install-deps.sh` |
## Prerequisites

- **macOS**: Apple Silicon with Hypervisor.framework
- **Linux**: KVM support (`/dev/kvm` accessible)
