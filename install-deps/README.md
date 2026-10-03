# nanosandbox install-deps

Runtime dependency packaging and installation for [nanosandbox](https://github.com/nanosandboxai/nanosandbox).

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

### Windows (PowerShell as Administrator)

```powershell
irm https://github.com/nanosandboxai/nanosandbox/releases/latest/download/install-deps.ps1 | iex
```

#### Options

```powershell
# Install a specific version
.\install-deps.ps1 -Version v0.3.0

# Custom install path
.\install-deps.ps1 -InstallDir C:\opt\nanosandbox
```

The installer will:
1. Check Windows version and Hyper-V status
2. Offer to enable Hyper-V if not active (requires reboot)
3. Download and install `libkrunfw.dll` and `busybox`
4. Add the install directory to the system PATH

## Uninstall

### Linux / macOS

```bash
curl -fsSL https://github.com/nanosandboxai/nanosandbox/releases/latest/download/uninstall-deps.sh | bash
```

### Windows (PowerShell as Administrator)

```powershell
irm https://github.com/nanosandboxai/nanosandbox/releases/latest/download/uninstall-deps.ps1 | iex
```

The uninstaller will:
1. Remove `libkrunfw.dll` and `busybox` from the install directory
2. Clean the install directory from the system PATH
3. Optionally remove VHDX and image caches

## How it works

1. The nanosandbox release pipeline (`release.yml`) builds libkrunfw + gvproxy (Linux/macOS) or libkrunfw.dll + busybox (Windows).
2. The dependency bundles (`deps-*`) and these installer scripts are published together in the same GitHub Release as the CLI.
3. `install-deps.sh` / `install-deps.ps1` download the bundle for the current platform from that release and install it under `~/.nanosandbox/`.

## Platform support

| Platform | Architecture | Bundle | Script |
|----------|-------------|--------|--------|
| Linux | x86_64 | `deps-linux-amd64.tar.gz` | `install-deps.sh` |
| Linux | aarch64 | `deps-linux-arm64.tar.gz` | `install-deps.sh` |
| macOS | Apple Silicon | `deps-darwin-arm64.tar.gz` | `install-deps.sh` |
| Windows | x86_64 | `deps-windows-amd64.zip` | `install-deps.ps1` |

## Prerequisites

- **macOS**: Apple Silicon with Hypervisor.framework
- **Linux**: KVM support (`/dev/kvm` accessible)
- **Windows**: Windows 10 1809+ / Server 2019+ with Hyper-V enabled
