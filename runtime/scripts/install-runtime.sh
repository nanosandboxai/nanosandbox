#!/bin/bash
#
# Install libkrun runtime for Nanosandbox
#
# Usage: ./scripts/install-runtime.sh [--check-only]
#
# Supported platforms:
#   - macOS (Apple Silicon) - installs libkrun via Homebrew
#   - Linux (Ubuntu/Debian) - installs libkrun from source
#   - Linux (Fedora/RHEL)  - installs libkrun via dnf
#
# Runtime architecture:
#   libkrun FFI (direct VM management, pure-Rust image handling)
#   gvproxy (user-mode networking for VM outbound connectivity)
#
# No buildah, krunvm, or crun dependency is required -- image pulling and
# rootfs creation are handled entirely in Rust by the ImageManager component.

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

    info "Installing libkrun runtime for macOS Apple Silicon..."

    # Check for Homebrew
    if ! command -v brew &> /dev/null; then
        error "Homebrew is required. Install from https://brew.sh"
    fi

    if $check_only; then
        info "Would install: libkrun via Homebrew (slp/krun tap)"
        return 0
    fi

    # Tap and install
    info "Adding slp/krun tap..."
    brew tap slp/krun || warn "Tap may already exist"

    # Install libkrun (direct FFI backend)
    info "Installing libkrun..."
    brew install slp/krun/libkrun || warn "libkrun may already be installed"

    # Verify libkrun installation
    if [[ -f "/opt/homebrew/lib/libkrun.dylib" ]]; then
        info "libkrun installed successfully: /opt/homebrew/lib/libkrun.dylib"
    else
        warn "libkrun.dylib not found at /opt/homebrew/lib/"
        warn "Installation may have failed"
    fi

    # Check HVF support
    info "Checking Hypervisor.framework support..."
    if sysctl -n kern.hv_support 2>/dev/null | grep -q "1"; then
        info "Hypervisor.framework available"
    else
        warn "Hypervisor.framework may not be available"
    fi

    info ""
    info "IMPORTANT: Binaries using libkrun must be signed with the"
    info "com.apple.security.hypervisor entitlement to create VMs."
    info ""
    info "For nanosb built from source, sign with:"
    info "  codesign --entitlements entitlements.plist --force -s - target/debug/nanosb"
    info ""
}

# =============================================================================
# Linux Installation
# =============================================================================
install_linux() {
    info "Installing libkrun runtime for Linux..."

    # Check for package manager
    if command -v apt-get &> /dev/null; then
        install_linux_debian
    elif command -v dnf &> /dev/null; then
        install_linux_fedora
    else
        error "Unsupported Linux distribution. Please install libkrun manually from: https://github.com/containers/libkrun"
    fi
}

install_linux_debian() {
    info "Detected Debian/Ubuntu..."

    if $check_only; then
        info "Would install: libkrun (from source)"
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
        python3 \
        python3-pip \
        ninja-build \
        pkg-config \
        curl

    # Create temp directory for builds
    BUILD_DIR=$(mktemp -d)
    cd "$BUILD_DIR"

    # Install libkrun
    if ldconfig -p | grep -q libkrun; then
        info "libkrun already installed"
    else
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

    # Update library cache
    sudo ldconfig

    # Cleanup
    cd /
    rm -rf "$BUILD_DIR"

    # Verify libkrun
    if ldconfig -p | grep -q libkrun; then
        info "libkrun installed successfully"
    else
        warn "libkrun not found in library cache"
    fi

    # Verify libkrun shared library location
    local libkrun_found=false
    for path in /usr/lib/libkrun.so /usr/lib64/libkrun.so /usr/local/lib/libkrun.so \
                /usr/lib/x86_64-linux-gnu/libkrun.so /usr/lib/aarch64-linux-gnu/libkrun.so; do
        if [[ -f "$path" ]]; then
            info "libkrun found at: $path"
            libkrun_found=true
            break
        fi
    done
    if ! $libkrun_found; then
        warn "libkrun.so not found in expected locations"
        warn "The FFI backend may not detect it at runtime"
    fi
}

install_linux_fedora() {
    info "Detected Fedora/RHEL..."

    if $check_only; then
        info "Would install: libkrun via dnf"
        return 0
    fi

    # Fedora has packages available
    info "Installing libkrun..."
    sudo dnf install -y libkrun

    # Verify libkrun
    if ldconfig -p 2>/dev/null | grep -q libkrun; then
        info "libkrun installed successfully"
    else
        warn "libkrun not found in library cache after install"
    fi
}

# =============================================================================
# gvproxy Installation (cross-platform)
# =============================================================================
install_gvproxy() {
    info "Installing gvproxy for VM networking..."

    if command -v gvproxy &> /dev/null; then
        info "gvproxy is already installed: $(which gvproxy)"
        return 0
    fi

    if $check_only; then
        info "Would install: gvproxy from gvisor-tap-vsock releases"
        return 0
    fi

    local gvproxy_version="v0.8.7"
    local gvproxy_binary=""

    case "$OS" in
        Darwin)
            gvproxy_binary="gvproxy-darwin"
            ;;
        Linux)
            case "$ARCH" in
                x86_64|amd64)
                    gvproxy_binary="gvproxy-linux-amd64"
                    ;;
                aarch64|arm64)
                    gvproxy_binary="gvproxy-linux-arm64"
                    ;;
                *)
                    warn "No gvproxy binary available for architecture: $ARCH"
                    warn "VM outbound networking will be limited."
                    return 0
                    ;;
            esac
            ;;
        *)
            warn "No gvproxy binary available for OS: $OS"
            return 0
            ;;
    esac

    local gvproxy_url="https://github.com/containers/gvisor-tap-vsock/releases/download/${gvproxy_version}/${gvproxy_binary}"

    info "Downloading gvproxy ${gvproxy_version}..."
    local tmp_dir
    tmp_dir="$(mktemp -d)"
    local download_path="${tmp_dir}/gvproxy"

    if ! curl -fsSL "$gvproxy_url" -o "$download_path"; then
        warn "Failed to download gvproxy from ${gvproxy_url}"
        warn "VM outbound networking will be limited (TSI fallback)."
        warn "Install manually from: https://github.com/containers/gvisor-tap-vsock/releases"
        rm -rf "$tmp_dir"
        return 0
    fi

    chmod +x "$download_path"

    # Install to a suitable location
    if [[ -w "/usr/local/bin" ]]; then
        sudo install -m 0755 "$download_path" /usr/local/bin/gvproxy
        info "gvproxy installed at /usr/local/bin/gvproxy"
    elif [[ "$OS" == "Darwin" && -w "/opt/homebrew/bin" ]]; then
        install -m 0755 "$download_path" /opt/homebrew/bin/gvproxy
        info "gvproxy installed at /opt/homebrew/bin/gvproxy"
    else
        mkdir -p "$HOME/.local/bin"
        install -m 0755 "$download_path" "$HOME/.local/bin/gvproxy"
        info "gvproxy installed at $HOME/.local/bin/gvproxy"
        if ! echo "$PATH" | tr ':' '\n' | grep -q "$HOME/.local/bin"; then
            warn "Add $HOME/.local/bin to your PATH for automatic detection"
        fi
    fi

    rm -rf "$tmp_dir"
}

# =============================================================================
# Main
# =============================================================================
case "$OS" in
    Darwin)
        install_macos
        install_gvproxy
        ;;
    Linux)
        install_linux
        install_gvproxy
        ;;
    *)
        error "Unsupported operating system: $OS"
        ;;
esac

info "Runtime installation complete!"
info ""
info "Runtime architecture:"
info "  Backend:    libkrun FFI (direct VM management)"
info "  Networking: gvproxy (user-mode virtio-net for outbound connectivity)"
info "  Images:     Pure-Rust ImageManager (no external tools needed)"
info ""
info "Next steps:"
info "  1. Run './scripts/check-e2e-prereqs.sh' to verify installation"
info "  2. Run 'make test-e2e' to run E2E tests"
