# nanosandbox install-deps

Runtime dependency packaging and installation for [nanosandbox](https://github.com/nanosandboxai/runtime).

## What gets installed

### Linux / macOS

| Component | Description | Install path |
|-----------|-------------|-------------|
| **libkrunfw** | Kernel firmware loaded at VM boot | `/usr/local/lib/` |
| **gvproxy** | User-mode networking daemon | `~/.local/bin/` |

### Windows

| Component | Description | Install path |
|-----------|-------------|-------------|
| **libkrunfw.dll** | Kernel firmware (DLL with embedded vmlinux) | `%ProgramFiles%\nanosandbox\` |
| **busybox** | Static Linux binary for initrd generation | `%ProgramFiles%\nanosandbox\` |

## Install

### Linux / macOS

```bash
curl -fsSL https://github.com/nanosandboxai/install-deps/releases/latest/download/install.sh | bash
```

#### Options

```bash
# Install a specific version
DEPS_VERSION=v0.2.0 bash install.sh

# Custom install paths
LIB_DIR=/opt/nanosandbox/lib BIN_DIR=/opt/nanosandbox/bin bash install.sh
```

### Windows (PowerShell as Administrator)

```powershell
irm https://github.com/nanosandboxai/install-deps/releases/latest/download/install.ps1 | iex
```

#### Options

```powershell
# Install a specific version
.\install.ps1 -Version v0.3.0

# Custom install path
.\install.ps1 -InstallDir C:\opt\nanosandbox
```

The installer will:
1. Check Windows version and Hyper-V status
2. Offer to enable Hyper-V if not active (requires reboot)
3. Download and install `libkrunfw.dll` and `busybox`
4. Add the install directory to the system PATH

## Uninstall

### Linux / macOS

```bash
curl -fsSL https://github.com/nanosandboxai/install-deps/releases/latest/download/uninstall.sh | bash
```

### Windows (PowerShell as Administrator)

```powershell
irm https://github.com/nanosandboxai/install-deps/releases/latest/download/uninstall.ps1 | iex
```

The uninstaller will:
1. Remove `libkrunfw.dll` and `busybox` from the install directory
2. Clean the install directory from the system PATH
3. Optionally remove VHDX and image caches

## How it works

1. **runtime** repo builds libkrunfw + gvproxy (Linux/macOS) or libkrunfw.dll + busybox (Windows) during release CI
2. Runtime CI triggers `repository_dispatch` to this repo
3. This repo packages the deps with install scripts and creates a GitHub Release
4. Users (or the CLI installer) download and run `install.sh` / `install.ps1`

## Platform support

| Platform | Architecture | Bundle | Script |
|----------|-------------|--------|--------|
| Linux | x86_64 | `deps-linux-amd64.tar.gz` | `install.sh` |
| Linux | aarch64 | `deps-linux-arm64.tar.gz` | `install.sh` |
| macOS | Apple Silicon | `deps-darwin-arm64.tar.gz` | `install.sh` |
| Windows | x86_64 | `deps-windows-amd64.zip` | `install.ps1` |

## Prerequisites

- **macOS**: Apple Silicon with Hypervisor.framework
- **Linux**: KVM support (`/dev/kvm` accessible)
- **Windows**: Windows 10 1809+ / Server 2019+ with Hyper-V enabled
