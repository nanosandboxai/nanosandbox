#!/bin/bash
# Nanosandbox Runtime Dependencies Installer
#
# Installs: libkrunfw (kernel firmware) + gvproxy (networking daemon)
#
# All files are installed under ~/.nanosandbox/ (no sudo required):
#   ~/.nanosandbox/libs/  — shared libraries (libkrunfw)
#   ~/.nanosandbox/bin/   — binaries (gvproxy)
#
# Usage:
#   curl -fsSL https://github.com/nanosandboxai/install-deps/releases/latest/download/install.sh | bash
#
# After install, open a new terminal (or run `source ~/.zshrc`) to pick up PATH.
#
# Environment variables:
#   DEPS_VERSION  - Version to install (default: latest)
#   NANOSANDBOX_HOME - Base directory (default: ~/.nanosandbox)

set -euo pipefail

# ─── Configuration ───────────────────────────────────────────────────────────

GITHUB_REPO="nanosandboxai/install-deps"
NANOSANDBOX_HOME="${NANOSANDBOX_HOME:-$HOME/.nanosandbox}"
LIB_DIR="${NANOSANDBOX_HOME}/libs"
BIN_DIR="${NANOSANDBOX_HOME}/bin"

# ─── Helpers ─────────────────────────────────────────────────────────────────

if [ -t 1 ]; then
    GREEN=$'\033[0;32m'; YELLOW=$'\033[1;33m'
    RED=$'\033[0;31m'; BLUE=$'\033[0;34m'; NC=$'\033[0m'
else
    GREEN=""; YELLOW=""; RED=""; BLUE=""; NC=""
fi

info()    { printf '  %s\n' "$1"; }
success() { printf '  %s[OK]%s %s\n' "$GREEN" "$NC" "$1"; }
warn()    { printf '  %s[WARN]%s %s\n' "$YELLOW" "$NC" "$1"; }
error()   { printf '  %s[ERROR]%s %s\n' "$RED" "$NC" "$1" >&2; }
header()  { printf '\n%s==>%s %s\n' "$BLUE" "$NC" "$1"; }

# ─── Platform detection ──────────────────────────────────────────────────────

detect_platform() {
    local raw_os raw_arch
    raw_os="$(uname -s)"
    raw_arch="$(uname -m)"

    case "$raw_os" in
        Linux)  OS="linux" ;;
        Darwin) OS="darwin" ;;
        *)      error "Unsupported OS: $raw_os"; exit 1 ;;
    esac

    case "$raw_arch" in
        x86_64|amd64)  ARCH="amd64" ;;
        aarch64|arm64) ARCH="arm64" ;;
        *)             error "Unsupported architecture: $raw_arch"; exit 1 ;;
    esac

    PLATFORM="${OS}-${ARCH}"
}

# ─── Prerequisites ───────────────────────────────────────────────────────────

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
            warn "/dev/kvm exists but is not accessible by current user"
            info "Run: sudo usermod -aG kvm $(whoami)"
        fi
        success "KVM available"
    fi

    command -v curl &>/dev/null || { error "curl is required but not installed"; exit 1; }
}

# ─── Version resolution ──────────────────────────────────────────────────────

# Resolve "latest" to the most recent release tag.
# Uses /releases/latest first (GitHub-sorted, honors "Set as latest release");
# falls back to /releases[0] for repos that only publish prereleases.
resolve_version() {
    if [ -n "${DEPS_VERSION:-}" ]; then
        VERSION="$DEPS_VERSION"
        info "Using specified version: $VERSION"
        return
    fi

    info "Resolving latest release tag..."

    local api="https://api.github.com/repos/${GITHUB_REPO}"
    local hdr="Accept: application/vnd.github+json"
    local resp tag

    # Try /releases/latest (stable + flagged-as-latest)
    resp="$(curl -fsSL -H "$hdr" "${api}/releases/latest" 2>&1)" || resp=""
    tag="$(printf '%s\n' "$resp" | sed -nE 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' | head -n1)"

    # Fall back to first entry in /releases (prerelease-only repos)
    if [ -z "$tag" ]; then
        resp="$(curl -fsSL -H "$hdr" "${api}/releases?per_page=1" 2>&1)" || resp=""
        tag="$(printf '%s\n' "$resp" | sed -nE 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' | head -n1)"
    fi

    if [ -z "$tag" ]; then
        error "Could not determine latest version from ${api}/releases"
        info "Set DEPS_VERSION=vX.Y.Z to pin a specific version"
        exit 1
    fi

    VERSION="$tag"
    info "Latest: $VERSION"
}

# ─── Download & install ──────────────────────────────────────────────────────

download_and_install() {
    header "Installing dependencies for ${PLATFORM}"

    local bundle="deps-${PLATFORM}.tar.gz"
    local url="https://github.com/${GITHUB_REPO}/releases/download/${VERSION}/${bundle}"
    local tmpdir
    tmpdir="$(mktemp -d)"
    trap 'rm -rf "${tmpdir:-}"' EXIT

    info "Downloading ${bundle}..."
    if ! curl -fsSL -o "${tmpdir}/${bundle}" "$url"; then
        error "Failed to download ${bundle}"
        info "URL: $url"
        info "Verify ${VERSION} has a ${PLATFORM} build at:"
        info "  https://github.com/${GITHUB_REPO}/releases/tag/${VERSION}"
        exit 1
    fi

    info "Extracting..."
    tar xzf "${tmpdir}/${bundle}" -C "$tmpdir"

    header "Installing libkrunfw"
    mkdir -p "$LIB_DIR"
    if [ "$OS" = "darwin" ]; then
        cp "$tmpdir"/lib/libkrunfw*.dylib "$LIB_DIR/"
    else
        cp "$tmpdir"/lib/libkrunfw*.so* "$LIB_DIR/"
    fi
    success "Installed to ${LIB_DIR}/"

    header "Installing gvproxy"
    mkdir -p "$BIN_DIR"
    cp "$tmpdir/bin/gvproxy" "$BIN_DIR/"
    chmod +x "$BIN_DIR/gvproxy"
    success "Installed to ${BIN_DIR}/gvproxy"
}

# ─── /usr/local/bin symlink ──────────────────────────────────────────────────

# Symlinks ~/.nanosandbox/bin/gvproxy into /usr/local/bin so it's on PATH in
# every new terminal without rc-file edits. /usr/local/bin is on the default
# PATH on both macOS and Linux. Requires sudo; falls back silently if not
# available (PATH edits in check_path() still cover the user's shell).
install_symlink() {
    local target="${BIN_DIR}/gvproxy"
    local link="/usr/local/bin/gvproxy"

    header "Linking gvproxy into /usr/local/bin"

    if [ ! -d /usr/local/bin ]; then
        sudo mkdir -p /usr/local/bin 2>/dev/null || {
            warn "Could not create /usr/local/bin — skipping symlink"
            info "gvproxy still available at ${target}"
            return 0
        }
    fi

    if sudo -n true 2>/dev/null; then
        sudo ln -sf "$target" "$link"
        success "Linked ${link} → ${target}"
    else
        info "sudo password required to link gvproxy into /usr/local/bin"
        if sudo ln -sf "$target" "$link"; then
            success "Linked ${link} → ${target}"
        else
            warn "Skipped /usr/local/bin symlink — gvproxy available at ${target}"
            info "Add ${BIN_DIR} to PATH manually or re-run installer with sudo"
        fi
    fi
}

# ─── PATH check ──────────────────────────────────────────────────────────────

check_path() {
    case ":$PATH:" in
        *":$BIN_DIR:"*) return ;;
    esac

    header "Configuring PATH"

    local line="export PATH=\"${BIN_DIR}:\$PATH\""
    local added=false

    for rc in "$HOME/.zshrc" "$HOME/.bashrc" "$HOME/.bash_profile"; do
        [ -f "$rc" ] || continue
        if ! grep -qF "$BIN_DIR" "$rc" 2>/dev/null; then
            printf '\n# Added by Nanosandbox installer\n%s\n' "$line" >> "$rc"
            success "Added to $(basename "$rc")"
            added=true
        fi
    done

    if [ "$added" = false ]; then
        # No shell profile found — create .zshrc on macOS, .bashrc on Linux
        local default_rc="$HOME/.bashrc"
        [ "$OS" = "darwin" ] && default_rc="$HOME/.zshrc"
        printf '\n# Added by Nanosandbox installer\n%s\n' "$line" >> "$default_rc"
        success "Added to $(basename "$default_rc")"
    fi

    export PATH="${BIN_DIR}:$PATH"
    info "PATH updated for this session"
}

# ─── Summary ─────────────────────────────────────────────────────────────────

print_summary() {
    header "Installation complete"
    cat <<EOF
  libkrunfw  → ${LIB_DIR}/
  gvproxy    → ${BIN_DIR}/gvproxy
  symlink    → /usr/local/bin/gvproxy
  version    → ${VERSION}
  platform   → ${PLATFORM}
EOF

    if [ ! -L /usr/local/bin/gvproxy ]; then
        case ":$PATH:" in
            *":$BIN_DIR:"*) ;;
            *)
                echo ""
                info "Open a new terminal, or run 'source ~/.zshrc' to use ${BIN_DIR} in this one."
                ;;
        esac
    fi
}

# ─── Main ────────────────────────────────────────────────────────────────────

main() {
    echo "Nanosandbox Runtime Dependencies Installer"
    echo "=========================================="

    detect_platform
    info "Platform: ${PLATFORM}"

    check_prerequisites
    resolve_version
    download_and_install
    install_symlink
    check_path
    print_summary
}

main "$@"
