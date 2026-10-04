#!/bin/bash
#
# Nanosandbox CLI Uninstaller
#
# Removes the nanosb CLI binary and (by default) its runtime dependencies.
#
#   1. Removes the nanosb binary from ~/.nanosandbox/bin/
#   2. Delegates dependency removal to the dependency uninstaller
#      (uninstall-deps.sh: libkrunfw + gvproxy)
#
# Usage:
#   curl -fsSL https://github.com/nanosandboxai/nanosandbox/releases/latest/download/uninstall.sh | bash
#
# Environment variables:
#   NANOSANDBOX_HOME - Base directory (default: ~/.nanosandbox)
#   DEPS_VERSION     - install-deps version to use for uninstall (default: latest)
#   KEEP_DEPS        - If set to "1", keep libkrunfw + gvproxy installed
#

set -eo pipefail

# =============================================================================
# Configuration
# =============================================================================

NANOSANDBOX_HOME="${NANOSANDBOX_HOME:-$HOME/.nanosandbox}"
INSTALL_DIR="${NANOSANDBOX_HOME}/bin"
DEPS_VERSION="${DEPS_VERSION:-latest}"
KEEP_DEPS="${KEEP_DEPS:-0}"

# Also check legacy location
LEGACY_INSTALL_DIR="$HOME/.local/bin"

# =============================================================================
# Helpers
# =============================================================================

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
# Step 1: Remove nanosb binary
# =============================================================================

remove_binary() {
    header "Removing nanosb binary"

    local binary_path="${INSTALL_DIR}/nanosb"

    if [[ -f "$binary_path" ]]; then
        rm -f "$binary_path"
        success "Removed ${binary_path}"
    else
        info "nanosb not found at ${binary_path}"
    fi

    # Check legacy location
    if [[ -f "${LEGACY_INSTALL_DIR}/nanosb" ]]; then
        rm -f "${LEGACY_INSTALL_DIR}/nanosb"
        info "Removed legacy ${LEGACY_INSTALL_DIR}/nanosb"
    fi

    # Remove /usr/local/bin symlink if it points at our nanosb. Only remove
    # symlinks (not real binaries) and only ones we own. If a real binary
    # exists at /usr/local/bin/nanosb, warn the user — that's not ours.
    sym="/usr/local/bin/nanosb"
    if [[ -L "$sym" ]]; then
        target="$(readlink "$sym" 2>/dev/null || true)"
        case "$target" in
            "${INSTALL_DIR}/nanosb"|"${LEGACY_INSTALL_DIR}/nanosb")
                sudo rm -f "$sym" 2>/dev/null || rm -f "$sym" 2>/dev/null || true
                success "Removed symlink $sym"
                ;;
            *)
                warn "Symlink $sym points to $target — leaving in place"
                ;;
        esac
    elif [[ -f "$sym" ]]; then
        warn "Found another nanosb at: $sym"
        warn "Remove manually with: sudo rm -f $sym"
    fi
}

# =============================================================================
# Step 2: Remove runtime dependencies (libkrunfw + gvproxy)
# =============================================================================

# Resolve "latest" to the most recent release tag, including prereleases.
# Required because GitHub's /releases/latest/ redirect only matches stable
# (non-prerelease) releases — if the repo has only prereleases, the /latest/
# URL returns 404.
resolve_deps_version() {
    local resolved
    resolved="$(curl -fsSL \
        -H "Accept: application/vnd.github+json" \
        "https://api.github.com/repos/nanosandboxai/nanosandbox/releases" \
        2>/dev/null \
        | grep -m1 '"tag_name":' \
        | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
    printf '%s' "$resolved"
}

remove_deps() {
    if [[ "$KEEP_DEPS" == "1" ]]; then
        header "Keeping runtime dependencies"
        info "KEEP_DEPS=1 -- leaving libkrunfw and gvproxy installed"
        return 0
    fi

    header "Runtime dependencies"

    # Always prompt unless KEEP_DEPS is explicitly set
    if [[ -t 0 ]]; then
        printf "  Also remove runtime dependencies (libkrunfw, gvproxy)? [y/N] "
        read -r answer
        if [[ ! "$answer" =~ ^[Yy] ]]; then
            info "Keeping runtime dependencies"
            return 0
        fi
    fi

    info "Removing runtime dependencies..."

    local resolved_version="$DEPS_VERSION"
    if [[ "$DEPS_VERSION" == "latest" ]]; then
        resolved_version="$(resolve_deps_version)"
        if [[ -z "$resolved_version" ]]; then
            warn "Could not resolve latest release tag"
            warn "See: https://github.com/nanosandboxai/nanosandbox/releases"
            return 0
        fi
        info "Resolved latest → ${resolved_version}"
    fi

    local deps_uninstall_url="https://github.com/nanosandboxai/nanosandbox/releases/download/${resolved_version}/uninstall-deps.sh"
    local tmpfile
    tmpfile="$(mktemp -t nanosb-uninstall-deps.XXXXXX)"

    info "Downloading dependency uninstaller..."
    if ! curl -fsSL "$deps_uninstall_url" -o "$tmpfile"; then
        warn "Could not download dependency uninstaller (release ${resolved_version})"
        warn "Remove libkrunfw and gvproxy manually if needed"
        warn "See: https://github.com/nanosandboxai/nanosandbox/releases"
        rm -f "$tmpfile"
        return 0
    fi

    info "Running dependency uninstaller..."
    if NANOSANDBOX_HOME="$NANOSANDBOX_HOME" bash "$tmpfile"; then
        success "Runtime dependencies removed"
    else
        warn "dependency uninstaller exited with an error"
        warn "You may need to remove libkrunfw and gvproxy manually"
        warn "See: https://github.com/nanosandboxai/nanosandbox/releases"
    fi
    rm -f "$tmpfile"
}

# =============================================================================
# Main
# =============================================================================

header "Nanosandbox Uninstaller"

remove_binary
remove_deps

header "Uninstall Complete"

if [[ "$KEEP_DEPS" == "1" ]]; then
    cat <<EOF
The nanosb CLI has been removed.
Runtime dependencies (libkrunfw + gvproxy) were kept (KEEP_DEPS=1).

EOF
else
    cat <<EOF
The nanosb CLI has been removed.
Runtime dependencies (libkrunfw + gvproxy) have been removed.

EOF
fi
