# Testing Guide

This document covers how to run tests for Nanosandbox, including unit tests, integration tests, and end-to-end (E2E) tests.

## Test Categories

| Category | Count | Dependencies | Command |
|----------|-------|--------------|---------|
| Unit Tests | 17 | None | `make test` |
| Integration Tests | 19 | None | `make test` |
| Network E2E | 5 | Network access | `make test-e2e-network` |
| Sandbox E2E | 1 | Runtime + KVM/HVF | `make test-e2e` |

## Quick Start

```bash
# Run all non-E2E tests
make test

# Run network-only E2E tests
make test-e2e-network

# Run all E2E tests (requires runtime)
make install-runtime  # First time only
make test-e2e
```

## Running Tests

### Unit and Integration Tests

These tests run without any external dependencies:

```bash
# Run all tests
make test

# Run only unit tests
make test-unit

# Run with verbose output
cargo test -- --nocapture
```

### E2E Tests

E2E tests are marked with `#[ignore]` and require additional setup.

#### Network-Only E2E Tests

These tests only require network access to Docker Hub:

```bash
make test-e2e-network
```

Tests included:
- `test_pull_alpine_image` - Pull alpine image from Docker Hub
- `test_create_rootfs` - Create rootfs from pulled layers
- `test_full_image_to_bundle_flow` - Full image to OCI bundle pipeline

#### Full E2E Tests

These tests require libkrun and KVM (Linux) or HVF (macOS):

```bash
make check-prereqs  # Verify prerequisites
make test-e2e       # Run all E2E tests
```

Tests included:
- All network tests above
- `test_sandbox_creation` - Create and manage a real sandbox

## Installing Runtime Dependencies

### Prerequisites Check

Before installing, check what's needed:

```bash
./scripts/check-e2e-prereqs.sh
```

### Automatic Installation

```bash
make install-runtime
```

This script handles installation for:
- **Linux (Ubuntu/Debian)**: Builds libkrun from source
- **Linux (Fedora)**: Installs libkrun via dnf
- **macOS (Apple Silicon)**: Installs libkrun via Homebrew

### Manual Installation

#### Linux (Ubuntu/Debian)

```bash
# Install build dependencies
sudo apt-get update
sudo apt-get install -y build-essential git libseccomp-dev libcap-dev \
    libsystemd-dev libyajl-dev go-md2man python3 ninja-build \
    pkg-config autoconf automake libtool

# Install Rust (if not present)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build and install libkrun
git clone https://github.com/containers/libkrun.git
cd libkrun
make
sudo make install
cd ..

# Update library cache
sudo ldconfig
```

#### Linux (Fedora/RHEL)

```bash
sudo dnf install -y libkrun
```

#### macOS (Apple Silicon)

```bash
# Install via Homebrew
brew tap slp/krun
brew install libkrun

# Verify HVF entitlement
codesign -d --entitlements :- $(which krun)
```

### Enabling KVM (Linux)

If `/dev/kvm` is not available:

```bash
# Load KVM modules
sudo modprobe kvm
sudo modprobe kvm_intel  # or kvm_amd for AMD CPUs

# Add to boot (Ubuntu)
echo "kvm" | sudo tee -a /etc/modules
echo "kvm_intel" | sudo tee -a /etc/modules

# Add user to kvm group
sudo usermod -aG kvm $USER
newgrp kvm
```

## CI/CD Configuration

### GitHub Actions

The project includes a GitHub Actions workflow (`.github/workflows/e2e.yml`) that runs:

1. **Unit Tests** - Runs on every push (GitHub-hosted runner)
2. **Network E2E Tests** - Runs on every push (GitHub-hosted runner)
3. **Sandbox E2E Tests** - Runs on main branch only (self-hosted runner with KVM)

### Self-Hosted Runner Setup

To run sandbox E2E tests in CI, set up a self-hosted runner with:

1. Linux with KVM enabled
2. libkrun installed
3. Runner labels: `self-hosted`, `linux`, `kvm`

```bash
# On your self-hosted runner
./scripts/install/linux.sh
./scripts/check-e2e-prereqs.sh
```

## Troubleshooting

### "KVM not available"

```bash
# Check if KVM is supported
egrep -c '(vmx|svm)' /proc/cpuinfo  # Should be > 0

# Load KVM modules
sudo modprobe kvm
sudo modprobe kvm_intel  # or kvm_amd

# Check /dev/kvm exists
ls -la /dev/kvm

# Fix permissions
sudo usermod -aG kvm $USER
```

### "libkrun.so not found"

The libkrun shared library is not installed. Install it:

```bash
# From source
git clone https://github.com/containers/libkrun.git
cd libkrun
make
sudo make install
sudo ldconfig
```

### "Cannot reach Docker Hub"

Check network connectivity:

```bash
curl -v https://registry-1.docker.io/v2/

# If behind proxy
export HTTP_PROXY=http://proxy:port
export HTTPS_PROXY=http://proxy:port
```

### "HVF entitlement not found" (macOS)

The krun binary needs the Hypervisor.framework entitlement:

```bash
# Check current entitlements
codesign -d --entitlements :- $(which krun)

# If missing, reinstall via Homebrew
brew reinstall krun
```

## Writing New Tests

### Unit Tests

Add to `src/*.rs` in `#[cfg(test)]` modules:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_my_feature() {
        // ...
    }
}
```

### Integration Tests

Add to `tests/integration_test.rs`:

```rust
#[test]
fn test_new_integration() {
    // No external dependencies
}
```

### E2E Tests

Add to `tests/integration_test.rs` with `#[ignore]`:

```rust
#[tokio::test]
#[ignore] // Run with: cargo test -- --ignored
async fn test_new_e2e_feature() {
    // Requires network and/or runtime
}
```

## Testing the TUI

The `nanosb` TUI (`nanosb-cli` crate, `src/tui/`) has a layered test suite.
Only the first layer runs in CI; the VM-backed layers are opt-in.

| Layer | What it proves | Needs TTY/VM? | Command |
|-------|----------------|---------------|---------|
| Frame tests (`TestBackend`) | Renderer draws welcome, panels (loading/error/terminal), grid, sidebars, focus, help overlay, status/popup | No | `cargo test -p nanosb-cli` |
| Handler tests | `/add`, `/close`, `/open`, `/focus`, `/kill`, `/env`, `/theme`, `/zoom`, `/reconnect`, `/clearhistory` mutate `App` correctly; removed subcommands are rejected | No | `cargo test -p nanosb-cli` |
| Event-loop test | The headless core (`run::handle_event`) survives 100+ synthetic events and renders | No | `cargo test -p nanosb-cli` |
| Scripted VM E2E | Real supervised alpine sandbox: add → console attach → data → reconnect → kill → sandbox gone | Yes (VM) | `cargo test -p nanosb-cli tui::vm_test -- --ignored --nocapture` |
| Pty smoke harness | The TUI boots under a real PTY, accepts `/help`, renders, and exits on `/quit` | Yes (PTY + VM) | `scripts/tui-smoke.sh` |

### Headless layers (CI)

```bash
cargo test -p nanosb-cli
```

These construct `App` state directly and render into a
`ratatui::backend::TestBackend`; no terminal, no sandbox. `run::handle_event`
was extracted from the main loop exactly so this is possible.

### Scripted VM end-to-end (opt-in)

```bash
# Requires libkrun/libkrunfw, gvproxy, and the Hypervisor entitlement.
cargo build -p nanosb-cli
NANOSB_BINARY_PATH="$PWD/target/debug/nanosb" \
  cargo test -p nanosb-cli tui::vm_test -- --ignored --nocapture
```

The test boots a real `alpine:latest` supervisor, drives the extracted event
loop, and asserts the sandbox is no longer running after `/kill` (the same
mechanism `nanosb ps` uses: `SupervisorClient::is_running()`).

### Pty smoke harness (opt-in)

```bash
scripts/tui-smoke.sh                 # uses target/debug/nanosb
scripts/tui-smoke.sh --binary /path/to/nanosb
```

The harness builds the CLI, runs the TUI from an empty directory (welcome
screen, so no microVM boot), launches it under a real PTY (`expect` when
available, else `script -q`), sends `/help` then `/quit`, captures the
transcript, and asserts the `/help` overlay rendered. It prints
`PASS: tui-smoke.sh` or `FAIL: …` and cleans up the temporary directory on
exit. Interactive panel attach (which the pty harness does not exercise) is
covered by the `#[ignore]` VM test above.

## Makefile Reference

```bash
make help             # Show all targets
make build            # Build debug
make build-release    # Build release
make test             # Run unit/integration tests
make test-unit        # Run only unit tests
make test-e2e         # Run all E2E tests
make test-e2e-network # Run network-only E2E tests
make install-runtime  # Install libkrun
make check-prereqs    # Check E2E prerequisites
make clippy           # Run linter
make fmt              # Format code
make lint             # Run all linters
make clean            # Clean build artifacts
make ci               # Run full CI pipeline locally
```
