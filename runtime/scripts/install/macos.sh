#!/bin/bash
#
# Install Nanosandbox runtime dependencies for macOS
#
# This script installs everything needed to run nanosandbox on macOS Apple Silicon:
#   - Homebrew (if not present)
#   - libkrun (microVM runtime via Homebrew)
#   - gvproxy (user-mode networking for VM connectivity)
#   - Codesigns nanosb binary with Hypervisor.framework entitlement
#
# Usage: ./scripts/install/macos.sh
#
# Requirements: macOS 11+ on Apple Silicon (M1/M2/M3/M4)
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

header "Nanosandbox macOS Installer"

if [[ "$(uname)" != "Darwin" ]]; then
    error "This installer only supports macOS. For Linux, use: scripts/install/linux.sh"
fi

ARCH="$(uname -m)"
if [[ "$ARCH" != "arm64" ]]; then
    error "Nanosandbox requires Apple Silicon (M1/M2/M3/M4). Detected: $ARCH"
fi
success "Running on macOS Apple Silicon ($ARCH)"

# Check Hypervisor.framework
if sysctl -n kern.hv_support 2>/dev/null | grep -q "1"; then
    success "Hypervisor.framework is available"
else
    warn "Hypervisor.framework may not be available"
    warn "VM creation will likely fail without it"
fi

# =============================================================================
# Homebrew
# =============================================================================

header "Checking Homebrew"

if command -v brew &>/dev/null; then
    success "Homebrew is already installed: $(brew --version | head -1)"
else
    info "Installing Homebrew..."
    /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

    # Add Homebrew to PATH for Apple Silicon
    if [[ -f "/opt/homebrew/bin/brew" ]]; then
        eval "$(/opt/homebrew/bin/brew shellenv)"
    fi

    if command -v brew &>/dev/null; then
        success "Homebrew installed successfully"
    else
        error "Homebrew installation failed"
    fi
fi

# =============================================================================
# libkrun
# =============================================================================

header "Installing libkrun"

if [[ -f "/opt/homebrew/lib/libkrun.dylib" ]]; then
    success "libkrun is already installed at /opt/homebrew/lib/libkrun.dylib"
else
    info "Adding slp/krun tap..."
    brew tap slp/krun || warn "Tap may already exist"

    info "Installing libkrun (this may take a few minutes)..."
    brew install slp/krun/libkrun

    if [[ -f "/opt/homebrew/lib/libkrun.dylib" ]]; then
        success "libkrun installed successfully"
    else
        error "libkrun.dylib not found after install"
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
# Codesign nanosb binary (if found)
# =============================================================================

header "Codesigning nanosb"

sign_binary() {
    local binary_path="$1"

    if [[ ! -f "$binary_path" ]]; then
        return 1
    fi

    # Check if already signed with HVF entitlement
    if codesign -d --entitlements :- "$binary_path" 2>&1 | grep -q "com.apple.security.hypervisor"; then
        success "nanosb already signed with HVF entitlement: $binary_path"
        return 0
    fi

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
        success "Signed nanosb with com.apple.security.hypervisor: $binary_path"
    else
        warn "Failed to sign $binary_path"
        warn "Sign manually: codesign --entitlements entitlements.plist --force -s - $binary_path"
    fi

    rm -f "$entitlements_plist"
    return 0
}

# Try common locations
signed=false
for candidate in \
    "$(which nanosb 2>/dev/null || true)" \
    "./target/debug/nanosb" \
    "./target/release/nanosb" \
    "$HOME/.local/bin/nanosb" \
    "/usr/local/bin/nanosb"; do
    if [[ -n "$candidate" && -f "$candidate" ]]; then
        sign_binary "$candidate" && signed=true
        break
    fi
done

if [[ "$signed" != "true" ]]; then
    info "nanosb binary not found — skipping codesign"
    info "After building, sign with:"
    info "  codesign --entitlements entitlements.plist --force -s - target/debug/nanosb"
fi

# =============================================================================
# Summary
# =============================================================================

header "Installation Complete"

echo "Runtime architecture:"
echo "  Backend:    libkrun FFI (direct VM management via Hypervisor.framework)"
echo "  Networking: gvproxy (user-mode virtio-net for outbound connectivity)"
echo "  Images:     Pure-Rust ImageManager (no external tools needed)"
echo ""
echo "Run 'nanosb doctor' to verify the installation."
echo ""
