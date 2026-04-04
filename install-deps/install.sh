#!/bin/bash
# Nanosandbox Runtime Dependencies Installer
#
# Installs: libkrunfw (kernel firmware) + gvproxy (networking daemon)
#
# Usage:
#   curl -fsSL https://github.com/nanosandboxai/install-deps/releases/latest/download/install.sh | bash
#
# Environment variables:
#   DEPS_VERSION  - Version to install (default: latest)
#   LIB_DIR       - Library install path (default: /usr/local/lib)
#   BIN_DIR       - Binary install path (default: ~/.local/bin)

set -euo pipefail

# ─── Configuration ───

GITHUB_REPO="nanosandboxai/install-deps"
LIB_DIR="${LIB_DIR:-/usr/local/lib}"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"

# ─── Helpers ───

info()    { echo "  $1"; }
success() { echo "  [OK] $1"; }
error()   { echo "  [ERROR] $1" >&2; }
header()  { echo ""; echo "==> $1"; }

detect_platform() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "$OS" in
        Linux)  OS="linux" ;;
        Darwin) OS="darwin" ;;
        *)      error "Unsupported OS: $OS"; exit 1 ;;
    esac

    case "$ARCH" in
        x86_64|amd64)   ARCH="amd64" ;;
        aarch64|arm64)  ARCH="arm64" ;;
        *)              error "Unsupported architecture: $ARCH"; exit 1 ;;
    esac

    PLATFORM="${OS}-${ARCH}"
}

check_prerequisites() {
    header "Checking prerequisites"

    if [ "$OS" = "darwin" ]; then
        if ! sysctl -n kern.hv_support 2>/dev/null | grep -q 1; then
            error "Hypervisor.framework not available (Apple Silicon required)"
            exit 1
        fi
        success "Hypervisor.framework available"
    fi

    if [ "$OS" = "linux" ]; then
        if [ ! -e /dev/kvm ]; then
            error "/dev/kvm not found — KVM support required"
            info "Enable KVM in your kernel or BIOS settings"
            exit 1
        fi
        if [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
            info "/dev/kvm exists but is not accessible by current user"
            info "Run: sudo usermod -aG kvm $(whoami)"
        fi
        success "KVM available"
    fi

    if ! command -v curl &>/dev/null; then
        error "curl is required but not installed"
        exit 1
    fi
}

resolve_version() {
    if [ -n "${DEPS_VERSION:-}" ]; then
        VERSION="$DEPS_VERSION"
        info "Using specified version: $VERSION"
    else
        info "Resolving latest version..."
        VERSION=$(curl -fsSL -o /dev/null -w '%{url_effective}' \
            "https://github.com/${GITHUB_REPO}/releases/latest" | \
            grep -oE '[^/]+$')
        if [ -z "$VERSION" ]; then
            error "Could not determine latest version"
            exit 1
        fi
        info "Latest version: $VERSION"
    fi
}

download_and_install() {
    header "Installing dependencies for ${PLATFORM}"

    BUNDLE_NAME="deps-${PLATFORM}.tar.gz"
    DOWNLOAD_URL="https://github.com/${GITHUB_REPO}/releases/download/${VERSION}/${BUNDLE_NAME}"

    TMPDIR=$(mktemp -d)
    trap 'rm -rf "$TMPDIR"' EXIT

    info "Downloading ${BUNDLE_NAME}..."
    if ! curl -fsSL -o "${TMPDIR}/${BUNDLE_NAME}" "$DOWNLOAD_URL"; then
        error "Failed to download ${BUNDLE_NAME}"
        error "URL: ${DOWNLOAD_URL}"
        error "Check that version ${VERSION} exists and has a ${PLATFORM} build"
        exit 1
    fi

    info "Extracting..."
    tar xzf "${TMPDIR}/${BUNDLE_NAME}" -C "$TMPDIR"

    # Install libkrunfw
    header "Installing libkrunfw"
    if [ "$OS" = "darwin" ]; then
        sudo mkdir -p "$LIB_DIR"
        sudo cp "$TMPDIR"/lib/libkrunfw*.dylib "$LIB_DIR/"
        success "Installed to ${LIB_DIR}/"
        ls -la "${LIB_DIR}"/libkrunfw* 2>/dev/null || true
    elif [ "$OS" = "linux" ]; then
        sudo mkdir -p "$LIB_DIR"
        sudo cp "$TMPDIR"/lib/libkrunfw*.so* "$LIB_DIR/"
        sudo ldconfig 2>/dev/null || true
        success "Installed to ${LIB_DIR}/"
        ls -la "${LIB_DIR}"/libkrunfw* 2>/dev/null || true
    fi

    # Install gvproxy
    header "Installing gvproxy"
    mkdir -p "$BIN_DIR"
    cp "$TMPDIR/bin/gvproxy" "$BIN_DIR/"
    chmod +x "$BIN_DIR/gvproxy"
    success "Installed to ${BIN_DIR}/gvproxy"
}

check_path() {
    if [[ ":$PATH:" != *":$BIN_DIR:"* ]]; then
        echo ""
        info "NOTE: ${BIN_DIR} is not in your PATH"
        info "Add to your shell profile:"
        info "  export PATH=\"${BIN_DIR}:\$PATH\""
    fi
}

print_summary() {
    header "Installation complete"
    echo ""
    echo "  Installed components:"
    echo "    libkrunfw  → ${LIB_DIR}/"
    echo "    gvproxy    → ${BIN_DIR}/gvproxy"
    echo ""
    echo "  Version: ${VERSION}"
    echo "  Platform: ${PLATFORM}"
    echo ""
}

# ─── Main ───

main() {
    echo "Nanosandbox Runtime Dependencies Installer"
    echo "==========================================="

    detect_platform
    info "Platform: ${PLATFORM}"

    check_prerequisites
    resolve_version
    download_and_install
    check_path
    print_summary
}

main "$@"
