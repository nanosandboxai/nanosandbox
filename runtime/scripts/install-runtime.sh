#!/bin/bash
#
# Install crun + libkrun for Nanosandbox E2E tests
#
# Usage: ./scripts/install-runtime.sh [--check-only]
#
# Supported platforms:
#   - Linux (Ubuntu/Debian) - installs crun with libkrun
#   - macOS (Apple Silicon) - installs krun via Homebrew

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

error() {
    echo -e "${RED}[ERROR]${NC} $1"
    exit 1
}

check_only=false
if [[ "$1" == "--check-only" ]]; then
    check_only=true
fi

# Detect OS
OS="$(uname -s)"
ARCH="$(uname -m)"

info "Detected OS: $OS, Architecture: $ARCH"

# =============================================================================
# macOS Installation
# =============================================================================
install_macos() {
    if [[ "$ARCH" != "arm64" ]]; then
        error "macOS support requires Apple Silicon (arm64). Detected: $ARCH"
    fi

    info "Installing krunvm for macOS Apple Silicon..."

    # Check for Homebrew
    if ! command -v brew &> /dev/null; then
        error "Homebrew is required. Install from https://brew.sh"
    fi

    if $check_only; then
        info "Would install: krunvm via Homebrew (slp/krun tap)"
        return 0
    fi

    # Tap and install krunvm
    info "Adding slp/krun tap..."
    brew tap slp/krun || warn "Tap may already exist"

    info "Installing krunvm..."
    brew install slp/krun/krunvm || warn "krunvm may already be installed"

    # Verify installation
    if command -v krunvm &> /dev/null; then
        info "krunvm installed successfully: $(krunvm --version 2>&1 | head -1)"
    else
        error "krunvm installation failed"
    fi

    # Check HVF support
    info "Checking Hypervisor.framework support..."
    if sysctl -n kern.hv_support 2>/dev/null | grep -q "1"; then
        info "Hypervisor.framework available"
    else
        warn "Hypervisor.framework may not be available"
    fi

    # Check for case-sensitive volume
    info ""
    info "IMPORTANT: krunvm requires a case-sensitive APFS volume."
    info "If not already created, run:"
    info ""
    info "  diskutil apfs addVolume disk3 'Case-sensitive APFS' krunvm"
    info ""
    info "Then configure krunvm to use /Volumes/krunvm"
}

# =============================================================================
# Linux Installation
# =============================================================================
install_linux() {
    info "Installing crun with libkrun for Linux..."

    # Check for package manager
    if command -v apt-get &> /dev/null; then
        install_linux_debian
    elif command -v dnf &> /dev/null; then
        install_linux_fedora
    else
        error "Unsupported Linux distribution. Please install crun and libkrun manually."
    fi
}

install_linux_debian() {
    info "Detected Debian/Ubuntu..."

    if $check_only; then
        info "Would install: build dependencies, libkrun, crun"
        return 0
    fi

    # Check for KVM
    if [[ ! -e /dev/kvm ]]; then
        warn "/dev/kvm not found. Make sure KVM is enabled."
        warn "You may need to: sudo modprobe kvm && sudo modprobe kvm_intel (or kvm_amd)"
    fi

    # Install build dependencies
    info "Installing build dependencies..."
    sudo apt-get update
    sudo apt-get install -y \
        build-essential \
        git \
        libseccomp-dev \
        libcap-dev \
        libsystemd-dev \
        libyajl-dev \
        go-md2man \
        python3 \
        python3-pip \
        ninja-build \
        pkg-config \
        autoconf \
        automake \
        libtool \
        curl

    # Create temp directory for builds
    BUILD_DIR=$(mktemp -d)
    cd "$BUILD_DIR"

    # Check if libkrun is already installed
    if ldconfig -p | grep -q libkrun; then
        info "libkrun already installed"
    else
        # Install libkrun
        info "Building libkrun from source..."
        git clone https://github.com/containers/libkrun.git
        cd libkrun
        
        # Install Rust if not present
        if ! command -v cargo &> /dev/null; then
            info "Installing Rust..."
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
            source "$HOME/.cargo/env"
        fi
        
        make
        sudo make install
        cd ..
    fi

    # Check if crun with libkrun is already installed
    if crun --version 2>&1 | grep -q "libkrun"; then
        info "crun with libkrun already installed"
    else
        # Install crun with libkrun support
        info "Building crun with libkrun support..."
        git clone https://github.com/containers/crun.git
        cd crun
        ./autogen.sh
        ./configure --with-libkrun
        make
        sudo make install
        cd ..
    fi

    # Update library cache
    sudo ldconfig

    # Cleanup
    cd /
    rm -rf "$BUILD_DIR"

    # Verify installation
    if crun --version 2>&1 | grep -q "libkrun"; then
        info "crun with libkrun installed successfully"
        crun --version
    else
        warn "crun installed but libkrun support may not be enabled"
    fi
}

install_linux_fedora() {
    info "Detected Fedora/RHEL..."

    if $check_only; then
        info "Would install: crun, libkrun via dnf"
        return 0
    fi

    # Fedora has packages available
    info "Installing crun and libkrun..."
    sudo dnf install -y crun libkrun

    # Verify
    if crun --version &> /dev/null; then
        info "crun installed successfully"
        crun --version
    else
        error "crun installation failed"
    fi
}

# =============================================================================
# Main
# =============================================================================
case "$OS" in
    Darwin)
        install_macos
        ;;
    Linux)
        install_linux
        ;;
    *)
        error "Unsupported operating system: $OS"
        ;;
esac

info "Runtime installation complete!"
info ""
info "Next steps:"
info "  1. Run './scripts/check-e2e-prereqs.sh' to verify installation"
info "  2. Run 'make test-e2e' to run E2E tests"
