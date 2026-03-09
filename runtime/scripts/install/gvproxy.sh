#!/bin/bash
#
# Install gvproxy for Nanosandbox VM networking
#
# gvproxy provides user-mode networking (virtio-net) for outbound VM connectivity.
# Without it, VMs fall back to TSI networking (limited).
#
# Usage: ./scripts/install/gvproxy.sh
#
# Supports: macOS (Darwin), Linux (amd64, arm64)
# Installs to: ~/.local/bin/gvproxy (no sudo needed)
#

set -e

GVPROXY_VERSION="v0.8.7"
GVPROXY_REPO="https://github.com/containers/gvisor-tap-vsock/releases/download"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info()    { echo -e "${GREEN}[INFO]${NC} $1"; }
warn()    { echo -e "${YELLOW}[WARN]${NC} $1"; }
error()   { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }
success() { echo -e "${GREEN}[OK]${NC} $1"; }

# Check if already installed
if command -v gvproxy &>/dev/null; then
    success "gvproxy is already installed: $(which gvproxy)"
    exit 0
fi

# Also check ~/.local/bin directly (may not be in PATH)
if [[ -x "$HOME/.local/bin/gvproxy" ]]; then
    success "gvproxy is already installed: $HOME/.local/bin/gvproxy"
    if ! echo "$PATH" | tr ':' '\n' | grep -q "$HOME/.local/bin"; then
        warn "Add $HOME/.local/bin to your PATH for automatic detection"
    fi
    exit 0
fi

# Determine binary name for this platform
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Darwin)
        BINARY_NAME="gvproxy-darwin"
        ;;
    Linux)
        case "$ARCH" in
            x86_64|amd64)  BINARY_NAME="gvproxy-linux-amd64" ;;
            aarch64|arm64) BINARY_NAME="gvproxy-linux-arm64" ;;
            *) error "No gvproxy binary available for architecture: $ARCH" ;;
        esac
        ;;
    *)
        error "No gvproxy binary available for OS: $OS"
        ;;
esac

DOWNLOAD_URL="${GVPROXY_REPO}/${GVPROXY_VERSION}/${BINARY_NAME}"

info "Downloading gvproxy ${GVPROXY_VERSION} for ${OS} ${ARCH}..."

# Download to temp location
TMP_DIR="$(mktemp -d)"
DOWNLOAD_PATH="${TMP_DIR}/gvproxy"

if ! curl -fsSL "$DOWNLOAD_URL" -o "$DOWNLOAD_PATH"; then
    rm -rf "$TMP_DIR"
    error "Failed to download gvproxy from ${DOWNLOAD_URL}"
fi

chmod +x "$DOWNLOAD_PATH"

# Install to ~/.local/bin (no sudo needed)
INSTALL_DIR="$HOME/.local/bin"
mkdir -p "$INSTALL_DIR"
install -m 0755 "$DOWNLOAD_PATH" "${INSTALL_DIR}/gvproxy"

rm -rf "$TMP_DIR"

success "gvproxy installed at ${INSTALL_DIR}/gvproxy"

if ! echo "$PATH" | tr ':' '\n' | grep -q "$INSTALL_DIR"; then
    warn "Add ${INSTALL_DIR} to your PATH:"
    warn "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    warn "Add this to your ~/.bashrc or ~/.zshrc for persistence."
fi
