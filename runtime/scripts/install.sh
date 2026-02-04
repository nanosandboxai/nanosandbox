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
#   - krunvm (includes buildah and libkrun)
#   - nanosb CLI (from GitHub Releases)
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

install_krunvm() {
    header "Installing krunvm"
    
    if command -v krunvm &> /dev/null; then
        success "krunvm is already installed: $(krunvm --version)"
        info "Checking for updates..."
        brew upgrade krunvm 2>/dev/null || true
        return
    fi
    
    info "Adding slp/krun tap..."
    brew tap slp/krun
    
    info "Installing krunvm (this may take a few minutes)..."
    info "This will also install: buildah, libkrun, gpgme"
    brew install krunvm
    
    success "krunvm installed successfully"
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

    success "nanosb installed successfully: ${install_dir}/nanosb"
    info "nanosb version: $(${install_dir}/nanosb --version || echo 'unknown')"
}

# =============================================================================
# Verification
# =============================================================================

verify_installation() {
    header "Verifying Installation"
    
    local all_ok=true
    
    # Check krunvm
    if command -v krunvm &> /dev/null; then
        success "krunvm: $(krunvm --version)"
    else
        error "krunvm not found in PATH"
        all_ok=false
    fi
    
    # Check buildah
    if command -v buildah &> /dev/null; then
        success "buildah: $(buildah --version | head -1)"
    else
        error "buildah not found in PATH"
        all_ok=false
    fi
    
    # Quick krunvm test
    info "Testing krunvm..."
    if krunvm list &> /dev/null; then
        success "krunvm is working correctly"
    else
        warn "krunvm list failed - this may be normal on first run"
    fi

    # Check nanosb
    if command -v nanosb &> /dev/null; then
        success "nanosb: $(nanosb --version 2>/dev/null || echo 'unknown version')"
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
    echo "You can now use nanosandbox:"
    echo ""
    echo "  # Run a command in a sandbox"
    echo "  nanosb run alpine:3.19 echo 'Hello from sandbox!'"
    echo ""
    echo "  # Run with dd-agents image"
    echo "  nanosb run ghcr.io/devdone-labs/dd-agents:latest claude --version"
    echo ""
    echo "  # List running sandboxes"
    echo "  nanosb ps"
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
    
    # Pre-flight checks
    check_macos
    check_architecture
    check_hypervisor
    
    # Install dependencies
    install_homebrew
    install_krunvm
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
