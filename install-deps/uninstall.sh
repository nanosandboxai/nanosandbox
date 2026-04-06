#!/bin/bash
# Nanosandbox Runtime Dependencies Uninstaller
#
# Removes: libkrunfw + gvproxy
# Does NOT remove the nanosb CLI binary.
#
# Usage:
#   curl -fsSL https://github.com/nanosandboxai/install-deps/releases/latest/download/uninstall.sh | bash
#
# Environment variables:
#   LIB_DIR  - Library install path (default: /usr/local/lib)
#   BIN_DIR  - Binary install path (default: ~/.local/bin)

set -euo pipefail

LIB_DIR="${LIB_DIR:-/usr/local/lib}"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"

# ─── Helpers ─────────────────────────────────────────────────────────────────

if [ -t 1 ]; then
    GREEN=$'\033[0;32m'; BLUE=$'\033[0;34m'; NC=$'\033[0m'
else
    GREEN=""; BLUE=""; NC=""
fi

info()    { printf '  %s\n' "$1"; }
success() { printf '  %s[OK]%s %s\n' "$GREEN" "$NC" "$1"; }
header()  { printf '\n%s==>%s %s\n' "$BLUE" "$NC" "$1"; }

echo "Nanosandbox Runtime Dependencies Uninstaller"
echo "============================================"

# ─── Remove libkrunfw ────────────────────────────────────────────────────────

header "Removing libkrunfw"
removed=false
shopt -s nullglob
for f in "${LIB_DIR}"/libkrunfw*; do
    sudo rm -f "$f"
    info "Removed $f"
    removed=true
done
shopt -u nullglob

if $removed; then
    [ "$(uname -s)" = "Linux" ] && sudo ldconfig 2>/dev/null || true
    success "libkrunfw removed"
else
    info "libkrunfw not found in ${LIB_DIR}"
fi

# ─── Remove gvproxy ──────────────────────────────────────────────────────────

header "Removing gvproxy"
if [ -f "${BIN_DIR}/gvproxy" ]; then
    rm -f "${BIN_DIR}/gvproxy"
    success "gvproxy removed from ${BIN_DIR}"
else
    info "gvproxy not found in ${BIN_DIR}"
fi

# ─── Summary ─────────────────────────────────────────────────────────────────

header "Uninstall complete"
cat <<EOF
  Runtime dependencies have been removed.
  The nanosb CLI binary was NOT removed.
EOF
