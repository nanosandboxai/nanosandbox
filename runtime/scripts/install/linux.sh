#!/bin/bash
#
# Install Nanosandbox runtime dependencies for Linux
#
# This script installs everything needed to run nanosandbox on Linux:
#   - libkrun (microVM runtime)
#     - Debian/Ubuntu: built from source
#     - Fedora/RHEL: installed via dnf
#   - gvproxy (user-mode networking for VM connectivity)
#   - KVM check with helpful hints
#
# Usage: ./scripts/install/linux.sh
#
# Requirements: Linux with KVM support (x86_64 or aarch64)
#

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

info()    { echo -e "${BLUE}[INFO]${NC} $1"; }
success() { echo -e "${GREEN}[OK]${NC} $1"; }
warn()    { echo -e "${YELLOW}[WARN]${NC} $1"; }
error()   { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

header() {
    echo ""
    echo "========================================"
    echo "  $1"
    echo "========================================"
    echo ""
}

# =============================================================================
# Pre-flight checks
# =============================================================================

header "Nanosandbox Linux Installer"

if [[ "$(uname -s)" != "Linux" ]]; then
    error "This installer only supports Linux. For macOS, use: scripts/install/macos.sh"
fi

ARCH="$(uname -m)"
info "Detected architecture: $ARCH"

# Check KVM
if [[ -e /dev/kvm ]]; then
    if [[ -r /dev/kvm && -w /dev/kvm ]]; then
        success "KVM is available and accessible"
    else
        warn "KVM exists but may not be accessible"
        warn "Add your user to the kvm group: sudo usermod -aG kvm \$USER"
        warn "Then log out and log back in"
    fi
else
    warn "/dev/kvm not found — KVM may not be enabled"
    warn "Enable in BIOS/UEFI and load module: sudo modprobe kvm_intel (or kvm_amd)"
fi

# =============================================================================
# libkrun install functions
# =============================================================================

install_debian_libkrun() {
    info "Detected Debian/Ubuntu — building libkrun from source..."

    # Install build dependencies
    info "Installing build dependencies..."
    sudo apt-get update
    sudo apt-get install -y \
        build-essential \
        git \
        python3 \
        python3-pip \
        ninja-build \
        pkg-config \
        curl

    # Build in temp directory
    BUILD_DIR="$(mktemp -d)"
    cd "$BUILD_DIR"

    if ldconfig -p | grep -q libkrun; then
        success "libkrun already in library cache"
    else
        info "Cloning and building libkrun..."
        git clone https://github.com/containers/libkrun.git
        cd libkrun

        # Install Rust if not present
        if ! command -v cargo &>/dev/null; then
            info "Installing Rust..."
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
            source "$HOME/.cargo/env"
        fi

        make
        sudo make install
    fi

    # Update library cache
    sudo ldconfig

    # Cleanup
    cd /
    rm -rf "$BUILD_DIR"

    # Verify
    if ldconfig -p | grep -q libkrun; then
        success "libkrun installed successfully"
    else
        warn "libkrun not found in library cache after build"
    fi
}

install_fedora_libkrun() {
    info "Detected Fedora/RHEL — installing libkrun via dnf..."

    sudo dnf install -y libkrun

    if ldconfig -p 2>/dev/null | grep -q libkrun; then
        success "libkrun installed successfully"
    else
        warn "libkrun not found in library cache after install"
    fi
}

# =============================================================================
# Install libkrun
# =============================================================================

header "Installing libkrun"

# Check if already installed
libkrun_found=false
for path in /usr/lib/libkrun.so /usr/lib64/libkrun.so /usr/local/lib/libkrun.so \
            /usr/lib/x86_64-linux-gnu/libkrun.so /usr/lib/aarch64-linux-gnu/libkrun.so; do
    if [[ -f "$path" ]]; then
        success "libkrun already installed at: $path"
        libkrun_found=true
        break
    fi
done

if [[ "$libkrun_found" != "true" ]]; then
    if command -v apt-get &>/dev/null; then
        install_debian_libkrun
    elif command -v dnf &>/dev/null; then
        install_fedora_libkrun
    else
        error "Unsupported Linux distribution. Install libkrun manually: https://github.com/containers/libkrun"
    fi
fi

# =============================================================================
# gvproxy
# =============================================================================

header "Installing gvproxy"

# Delegate to gvproxy.sh (reusable, cross-platform)
if [[ -f "${SCRIPT_DIR}/gvproxy.sh" ]]; then
    bash "${SCRIPT_DIR}/gvproxy.sh"
else
    warn "gvproxy.sh not found at ${SCRIPT_DIR}/gvproxy.sh"
    warn "Install gvproxy manually from: https://github.com/containers/gvisor-tap-vsock/releases"
fi

# =============================================================================
# Summary
# =============================================================================

header "Installation Complete"

echo "Runtime architecture:"
echo "  Backend:    libkrun FFI (direct VM management via KVM)"
echo "  Networking: gvproxy (user-mode virtio-net for outbound connectivity)"
echo "  Images:     Pure-Rust ImageManager (no external tools needed)"
echo ""
echo "Run 'nanosb doctor' to verify the installation."
echo ""
