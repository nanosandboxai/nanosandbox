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
# Nanosandbox CLI installation (from GitHub Releases)
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

fetch_release_json() {
    local version="$1"
    local url
    if [[ -n "$version" ]]; then
        url="https://api.github.com/repos/${REPO}/releases/tags/${version}"
    else
        url="https://api.github.com/repos/${REPO}/releases/latest"
    fi
    curl -fsSL "$url"
}

find_asset_url() {
    local json="$1"
    local asset_name="$2"
    python3 - <<PY
import json, sys
data = json.loads(sys.stdin.read())
assets = data.get("assets", [])
names = [
    f"{asset_name}.tar.gz",
    f"{asset_name}.zip",
    asset_name,
]
for name in names:
    for asset in assets:
        if asset.get("name") == name:
            print(asset.get("browser_download_url"))
            sys.exit(0)
print("")
PY
}

download_and_install_nanosb() {
    header "Installing nanosb CLI"

    if command -v nanosb &> /dev/null; then
        success "nanosb is already installed: $(nanosb --version 2>/dev/null || echo 'unknown version')"
        return
    fi

    local version="${NANOSB_VERSION:-}"
    local asset_name="${NANOSB_ASSET_NAME:-$DEFAULT_ASSET_NAME}"

    info "Fetching release metadata for ${REPO}..."
    local json
    if ! json="$(fetch_release_json "$version")"; then
        error "Failed to fetch release metadata. Ensure the release exists."
        exit 1
    fi

    local tag_name
    tag_name="$(python3 - <<PY
import json, sys
data = json.loads(sys.stdin.read())
print(data.get("tag_name", "unknown"))
PY
<<< "$json")"
    if [[ "$tag_name" == "unknown" || -z "$tag_name" ]]; then
        error "Release metadata missing tag_name. Ensure a release exists for the version."
        exit 1
    fi

    local asset_url
    asset_url="$(find_asset_url "$json" "$asset_name")"
    if [[ -z "$asset_url" ]]; then
        error "Release asset not found for '${asset_name}'."
        error "Expected one of: ${asset_name}, ${asset_name}.tar.gz, ${asset_name}.zip"
        error "Ensure the release assets are published."
        exit 1
    fi

    info "Downloading nanosb from release ${tag_name}..."
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
