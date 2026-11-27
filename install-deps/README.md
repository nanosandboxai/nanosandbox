# nanosandbox install-deps

Runtime dependency packaging and installation for [nanosandbox](https://github.com/nanosandboxai/nanosandbox).

## What gets installed

### Linux / macOS

Runtime dependencies install under `$NANOSANDBOX_HOME` (default
`~/.nanosandbox`); no sudo is required.

| Component | Description | Install path |
|-----------|-------------|-------------|
| **libkrunfw** | Kernel firmware loaded at VM boot | `$NANOSANDBOX_HOME/libs/` (default `~/.nanosandbox/libs/`) |
| **gvproxy** | User-mode networking daemon | `$NANOSANDBOX_HOME/bin/gvproxy` (default `~/.nanosandbox/bin/gvproxy`) |

The installer symlinks `gvproxy` into `/usr/local/bin` when it can (so it is on
`PATH`), and otherwise adds `$NANOSANDBOX_HOME/bin` to your shell profile.

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
DEPS_VERSION=v0.3.0 curl -fsSL .../install-deps.sh | bash

# Custom install prefix
NANOSANDBOX_HOME=/opt/nanosandbox curl -fsSL .../install-deps.sh | bash
```

#### Verify

```bash
nanosb doctor
```

On a clean macOS arm64 install this prints (4 checks, none failing):

```
  [✓] Architecture: Apple Silicon (aarch64)
  [✓] libkrunfw Kernel Firmware: found (libkrunfw.5.dylib)
  [✓] Hypervisor.framework: available
  [✓] gvproxy: available (full outbound networking)

4 checks passed, 0 errors, 0 warnings
```

## Uninstall

### Linux / macOS

```bash
curl -fsSL https://github.com/nanosandboxai/nanosandbox/releases/latest/download/uninstall-deps.sh | bash
```

## How it works

1. The nanosandbox release pipeline (`release.yml`) builds libkrunfw + gvproxy (Linux/macOS).
2. The dependency bundles (`deps-*`) and these installer scripts are published together in the same GitHub Release as the CLI. The source scripts in this repo are `install-deps/install.sh` and `install-deps/uninstall.sh`; the release pipeline publishes them as `install-deps.sh` and `uninstall-deps.sh`.
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
