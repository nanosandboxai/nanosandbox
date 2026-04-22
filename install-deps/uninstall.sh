#!/bin/bash
# Nanosandbox Runtime Dependencies Uninstaller
#
# Removes: libkrunfw + gvproxy from ~/.nanosandbox/
# Does NOT remove the nanosb CLI binary.
#
# Usage:
#   curl -fsSL https://github.com/nanosandboxai/install-deps/releases/latest/download/uninstall.sh | bash
#
# Environment variables:
#   NANOSANDBOX_HOME - Base directory (default: ~/.nanosandbox)

set -euo pipefail

NANOSANDBOX_HOME="${NANOSANDBOX_HOME:-$HOME/.nanosandbox}"
LIB_DIR="${NANOSANDBOX_HOME}/libs"
BIN_DIR="${NANOSANDBOX_HOME}/bin"

# Also clean up legacy locations (pre-v0.2.0-rc5 installed here)
LEGACY_LIB_DIR="/usr/local/lib"
LEGACY_BIN_DIR="$HOME/.local/bin"

# ─── Helpers ─────────────────────────────────────────────────────────────────

if [ -t 1 ]; then
    GREEN=$'\033[0;32m'; YELLOW=$'\033[1;33m'; BLUE=$'\033[0;34m'; NC=$'\033[0m'
else
    GREEN=""; YELLOW=""; BLUE=""; NC=""
fi

info()    { printf '  %s\n' "$1"; }
success() { printf '  %s[OK]%s %s\n' "$GREEN" "$NC" "$1"; }
warn()    { printf '  %s[WARN]%s %s\n' "$YELLOW" "$NC" "$1"; }
header()  { printf '\n%s==>%s %s\n' "$BLUE" "$NC" "$1"; }

echo "Nanosandbox Runtime Dependencies Uninstaller"
echo "============================================"

# ─── Remove libkrunfw ────────────────────────────────────────────────────────

header "Removing libkrunfw"
removed=false
shopt -s nullglob

for f in "${LIB_DIR}"/libkrunfw*; do
    rm -f "$f"
    info "Removed $f"
    removed=true
done

# Clean up legacy location
for f in "${LEGACY_LIB_DIR}"/libkrunfw*; do
    sudo rm -f "$f" 2>/dev/null || true
    info "Removed legacy $f"
    removed=true
done

shopt -u nullglob

if $removed; then
    [ "$(uname -s)" = "Linux" ] && sudo ldconfig 2>/dev/null || true
    success "libkrunfw removed"
else
    info "libkrunfw not found"
fi

# ─── Remove gvproxy ──────────────────────────────────────────────────────────

header "Removing gvproxy"
gvproxy_removed=false

if [ -f "${BIN_DIR}/gvproxy" ]; then
    rm -f "${BIN_DIR}/gvproxy"
    success "gvproxy removed from ${BIN_DIR}"
    gvproxy_removed=true
fi

# Clean up legacy location
if [ -f "${LEGACY_BIN_DIR}/gvproxy" ]; then
    rm -f "${LEGACY_BIN_DIR}/gvproxy"
    info "Removed legacy ${LEGACY_BIN_DIR}/gvproxy"
    gvproxy_removed=true
fi

if ! $gvproxy_removed; then
    info "gvproxy not found"
fi

# ─── Summary ─────────────────────────────────────────────────────────────────

header "Uninstall complete"
cat <<EOF
  Runtime dependencies have been removed.
  The nanosb CLI binary was NOT removed.
EOF
