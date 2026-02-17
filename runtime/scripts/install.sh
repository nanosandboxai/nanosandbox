#!/bin/bash
# =============================================================================
# Nanosandbox macOS Installer
# =============================================================================
#
# This script installs all dependencies required to run nanosandbox on macOS
# Apple Silicon (M1/M2/M3/M4).
#
# Dependencies installed:
#   - Homebrew (if not present)
#   - libkrun (direct FFI backend)
#   - gvproxy (user-mode networking for VM outbound connectivity)
#   - nanosb CLI (from GitHub Releases)
#
# Architecture:
#   The runtime uses the direct libkrun FFI backend, which calls libkrun's
#   C API for microVM management and uses the pure-Rust ImageManager for
#   OCI image handling (no buildah or krunvm needed).
#
# Usage:
#   ./install.sh
#
# =============================================================================

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Print with color
info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

success() {
    echo -e "${GREEN}[OK]${NC} $1"
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

header() {
    echo ""
    echo "========================================"
    echo "  $1"
    echo "========================================"
    echo ""
}

# =============================================================================
# Config
# =============================================================================

REPO="devdone-labs/dd-nanosandbox"
DEFAULT_ASSET_NAME="nanosb-macos-arm64"
DEFAULT_RELEASE_TAG="v0.1.0"

# =============================================================================
# Pre-flight checks
# =============================================================================

check_macos() {
    header "Checking macOS"
    
    if [[ "$(uname)" != "Darwin" ]]; then
        error "This installer only supports macOS."
        error "Detected OS: $(uname)"
        error "For Linux, use: ./scripts/install-runtime.sh"
        exit 1
    fi
    success "Running on macOS"
}

check_architecture() {
    header "Checking Architecture"
    
    ARCH=$(uname -m)
    if [[ "$ARCH" != "arm64" ]]; then
        error "Nanosandbox requires Apple Silicon (M1/M2/M3/M4)."
        error "Detected architecture: $ARCH"
        exit 1
    fi
    success "Running on Apple Silicon ($ARCH)"
}

check_hypervisor() {
    header "Checking Hypervisor Framework"
    
    # Check if Hypervisor.framework is available
    if ! sysctl kern.hv_support 2>/dev/null | grep -q "1"; then
        warn "Hypervisor.framework may not be available."
        warn "This could indicate virtualization is disabled or not supported."
    else
        success "Hypervisor.framework is available"
    fi
}

# =============================================================================
# Installation functions
# =============================================================================

install_homebrew() {
    header "Checking Homebrew"
    
    if command -v brew &> /dev/null; then
        success "Homebrew is already installed: $(brew --version | head -1)"
        info "Updating Homebrew..."
        brew update
        return
    fi
    
    info "Installing Homebrew..."
    /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
    
    # Add Homebrew to PATH for Apple Silicon
    if [[ -f "/opt/homebrew/bin/brew" ]]; then
        eval "$(/opt/homebrew/bin/brew shellenv)"
    fi
    
    success "Homebrew installed successfully"
}

install_libkrun() {
    header "Installing libkrun"

    # Check if libkrun is already available
    if [[ -f "/opt/homebrew/lib/libkrun.dylib" ]]; then
        success "libkrun is already installed at /opt/homebrew/lib/libkrun.dylib"
        info "Checking for updates..."
        brew upgrade libkrun 2>/dev/null || true
        return
    fi

    info "Adding slp/krun tap..."
    brew tap slp/krun

    info "Installing libkrun (this may take a few minutes)..."
    brew install slp/krun/libkrun

    if [[ -f "/opt/homebrew/lib/libkrun.dylib" ]]; then
        success "libkrun installed successfully"
    else
        error "libkrun.dylib not found after install. Cannot proceed."
        exit 1
    fi
}

install_gvproxy() {
    header "Installing gvproxy"

    # Check if gvproxy is already installed
    if command -v gvproxy &> /dev/null; then
        success "gvproxy is already installed: $(which gvproxy)"
        return
    fi

    local gvproxy_version="v0.8.7"
    local gvproxy_url="https://github.com/containers/gvisor-tap-vsock/releases/download/${gvproxy_version}/gvproxy-darwin"

    info "Downloading gvproxy ${gvproxy_version} for macOS..."

    local tmp_dir
    tmp_dir="$(mktemp -d)"
    local download_path="${tmp_dir}/gvproxy"

    if ! curl -fsSL "$gvproxy_url" -o "$download_path"; then
        warn "Failed to download gvproxy from ${gvproxy_url}"
        warn "VM outbound networking will be limited (TSI fallback)."
        warn "You can install gvproxy manually from:"
        warn "  https://github.com/containers/gvisor-tap-vsock/releases"
        return
    fi

    chmod +x "$download_path"

    # Install to a suitable location
    local install_dir
    if [[ -w "/usr/local/bin" ]]; then
        install_dir="/usr/local/bin"
        sudo install -m 0755 "$download_path" "${install_dir}/gvproxy"
    elif [[ -w "/opt/homebrew/bin" ]]; then
        install_dir="/opt/homebrew/bin"
        install -m 0755 "$download_path" "${install_dir}/gvproxy"
    else
        install_dir="$HOME/.local/bin"
        mkdir -p "$install_dir"
        install -m 0755 "$download_path" "${install_dir}/gvproxy"
    fi

    rm -rf "$tmp_dir"

    if command -v gvproxy &> /dev/null || [[ -f "${install_dir}/gvproxy" ]]; then
        success "gvproxy installed at ${install_dir}/gvproxy"
    else
        warn "gvproxy installed at ${install_dir}/gvproxy but not in PATH."
        warn "Add ${install_dir} to your PATH for automatic detection."
    fi
}

# =============================================================================
# Nanosandbox CLI installation (from GitHub Releases, no auth)
# =============================================================================

select_install_dir() {
    if [[ -w "/usr/local/bin" ]]; then
        echo "/usr/local/bin"
        return
    fi
    if [[ -w "/opt/homebrew/bin" ]]; then
        echo "/opt/homebrew/bin"
        return
    fi
    mkdir -p "$HOME/.local/bin"
    echo "$HOME/.local/bin"
}

build_asset_urls() {
    local version="$1"
    local asset_name="$2"
    local base
    if [[ -n "$version" ]]; then
        base="https://github.com/${REPO}/releases/download/${version}"
    else
        base="https://github.com/${REPO}/releases/latest/download"
    fi

    printf '%s\n' \
        "${base}/${asset_name}.tar.gz" \
        "${base}/${asset_name}.zip" \
        "${base}/${asset_name}.dmg" \
        "${base}/${asset_name}"
}

download_and_install_nanosb() {
    header "Installing nanosb CLI"

    if command -v nanosb &> /dev/null; then
        success "nanosb is already installed: $(nanosb --version 2>/dev/null || echo 'unknown version')"
        return
    fi

    local version="${NANOSB_VERSION:-$DEFAULT_RELEASE_TAG}"
    local asset_name="${NANOSB_ASSET_NAME:-$DEFAULT_ASSET_NAME}"

    local asset_url=""
    while IFS= read -r candidate; do
        if curl -fsI "$candidate" >/dev/null 2>&1; then
            asset_url="$candidate"
            break
        fi
    done < <(build_asset_urls "$version" "$asset_name")

    if [[ -z "$asset_url" ]]; then
        error "Release asset not found for '${asset_name}'."
        if [[ -n "$version" ]]; then
            error "Ensure release '${version}' exists and includes the macOS asset."
        else
            error "Ensure the latest release includes the macOS asset."
        fi
        exit 1
    fi

    info "Downloading nanosb from release asset..."
    local tmp_dir
    tmp_dir="$(mktemp -d)"
    local download_path="${tmp_dir}/nanosb_asset"
    curl -fsSL "$asset_url" -o "$download_path"

    local bin_path=""
    if [[ "$asset_url" == *.tar.gz ]]; then
        tar -xzf "$download_path" -C "$tmp_dir"
        bin_path="$(find "$tmp_dir" -type f -name nanosb -maxdepth 2 | head -n 1)"
    elif [[ "$asset_url" == *.zip ]]; then
        unzip -q "$download_path" -d "$tmp_dir"
        bin_path="$(find "$tmp_dir" -type f -name nanosb -maxdepth 2 | head -n 1)"
    elif [[ "$asset_url" == *.dmg ]]; then
        local mount_dir
        mount_dir="$(mktemp -d)"
        if ! hdiutil attach "$download_path" -mountpoint "$mount_dir" -nobrowse -quiet; then
            error "Failed to mount DMG."
            exit 1
        fi
        bin_path="$(find "$mount_dir" -type f -name nanosb -maxdepth 3 | head -n 1)"
        hdiutil detach "$mount_dir" -quiet || true
    else
        bin_path="$download_path"
        chmod +x "$bin_path"
    fi

    if [[ -z "$bin_path" || ! -f "$bin_path" ]]; then
        error "Failed to locate nanosb binary in release asset."
        exit 1
    fi

    local install_dir
    install_dir="$(select_install_dir)"
    info "Installing nanosb to ${install_dir}..."
    if [[ "$install_dir" == "/usr/local/bin" ]]; then
        sudo install -m 0755 "$bin_path" "${install_dir}/nanosb"
    else
        install -m 0755 "$bin_path" "${install_dir}/nanosb"
    fi

    # Sign with Hypervisor.framework entitlement
    sign_with_hvf_entitlement "${install_dir}/nanosb"

    success "nanosb installed successfully: ${install_dir}/nanosb"
    info "nanosb version: $(${install_dir}/nanosb --version || echo 'unknown')"
}

sign_with_hvf_entitlement() {
    local binary_path="$1"

    info "Signing binary with Hypervisor.framework entitlement..."

    local entitlements_plist
    entitlements_plist="$(mktemp /tmp/nanosb-entitlements.XXXXXX.plist)"
    cat > "$entitlements_plist" << 'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.hypervisor</key>
    <true/>
</dict>
</plist>
PLIST

    if codesign --entitlements "$entitlements_plist" --force -s - "$binary_path" 2>/dev/null; then
        success "Binary signed with com.apple.security.hypervisor entitlement"
    else
        warn "Failed to sign binary with HVF entitlement."
        warn "You may need to sign manually:"
        warn "  codesign --entitlements entitlements.plist --force -s - $binary_path"
    fi

    rm -f "$entitlements_plist"
}

# =============================================================================
# Verification
# =============================================================================

verify_installation() {
    header "Verifying Installation"
    
    local all_ok=true
    
    # Check libkrun (required)
    if [[ -f "/opt/homebrew/lib/libkrun.dylib" ]]; then
        success "libkrun: /opt/homebrew/lib/libkrun.dylib"
    else
        error "libkrun.dylib not found -- runtime will not work"
        all_ok=false
    fi

    # Check gvproxy (optional but recommended)
    if command -v gvproxy &> /dev/null; then
        success "gvproxy: $(which gvproxy)"
    elif [[ -f "$HOME/.local/bin/gvproxy" ]]; then
        success "gvproxy: $HOME/.local/bin/gvproxy (not in PATH)"
    else
        warn "gvproxy not found -- VM outbound networking will be limited"
    fi

    # Check nanosb
    if command -v nanosb &> /dev/null; then
        success "nanosb: $(nanosb --version 2>/dev/null || echo 'unknown version')"

        # Check HVF entitlement on nanosb
        local nanosb_path
        nanosb_path="$(which nanosb)"
        if codesign -d --entitlements :- "$nanosb_path" 2>&1 | grep -q "com.apple.security.hypervisor"; then
            success "nanosb has Hypervisor.framework entitlement"
        else
            warn "nanosb may not have HVF entitlement (VM creation will fail)"
            warn "Sign with: codesign --entitlements entitlements.plist --force -s - $nanosb_path"
        fi
    else
        error "nanosb not found in PATH"
        all_ok=false
    fi
    
    if [[ "$all_ok" == "true" ]]; then
        return 0
    else
        return 1
    fi
}

print_summary() {
    header "Installation Complete"
    
    echo "Nanosandbox dependencies and CLI have been installed successfully!"
    echo ""
    echo "Runtime architecture:"
    echo "  Backend:    libkrun FFI (direct VM management via Hypervisor.framework)"
    echo "  Networking: gvproxy (user-mode virtio-net for full outbound connectivity)"
    echo "  Images:     Pure-Rust ImageManager (no buildah or krunvm needed)"
    echo ""
    echo "You can now use nanosandbox:"
    echo ""
    echo "  # Run a command in a sandbox"
    echo "  nanosb run alpine:3.19 -- echo 'Hello from sandbox!'"
    echo ""
    echo "  # Run with dd-agents image"
    echo "  nanosb run ghcr.io/devdone-labs/dd-agents:latest -- claude --version"
    echo ""
    echo "  # List running sandboxes"
    echo "  nanosb ps"
    echo ""
    echo "Note: The nanosb binary must be signed with the com.apple.security.hypervisor"
    echo "entitlement to use Hypervisor.framework. This was done automatically during"
    echo "installation. If you rebuild from source, re-sign with:"
    echo ""
    echo "  codesign --entitlements entitlements.plist --force -s - target/debug/nanosb"
    echo ""
    echo "For more information, see: https://github.com/devdone-labs/dd-nanosandbox"
    echo ""
}

# =============================================================================
# Main
# =============================================================================

main() {
    echo ""
    echo "========================================"
    echo "  Nanosandbox macOS Installer"
    echo "========================================"
    echo ""
    echo "This script will install all dependencies required"
    echo "to run nanosandbox on macOS Apple Silicon."
    echo ""
    echo "Components:"
    echo "  - libkrun  (FFI backend for microVM management)"
    echo "  - gvproxy  (user-mode networking for VM connectivity)"
    echo "  - nanosb   (CLI tool)"
    echo ""
    
    # Pre-flight checks
    check_macos
    check_architecture
    check_hypervisor
    
    # Install dependencies
    install_homebrew
    install_libkrun
    install_gvproxy
    download_and_install_nanosb
    
    # Verify
    if verify_installation; then
        print_summary
        exit 0
    else
        error "Installation verification failed."
        error "Please check the errors above and try again."
        exit 1
    fi
}

main "$@"
