#!/bin/bash
# Nanosandbox Runtime Dependencies Uninstaller
#
# Removes: libkrunfw + gvproxy
# Does NOT remove the nanosb CLI binary.
#
# Usage:
#   curl -fsSL https://github.com/nanosandboxai/install-deps/releases/latest/download/uninstall.sh | bash

set -euo pipefail

LIB_DIR="${LIB_DIR:-/usr/local/lib}"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"

info()    { echo "  $1"; }
success() { echo "  [OK] $1"; }
header()  { echo ""; echo "==> $1"; }

echo "Nanosandbox Runtime Dependencies Uninstaller"
echo "============================================="

# Remove libkrunfw
header "Removing libkrunfw"
removed=false
for f in "${LIB_DIR}"/libkrunfw*; do
    if [ -e "$f" ]; then
        sudo rm -f "$f"
        info "Removed $f"
        removed=true
    fi
done
if [ "$removed" = true ]; then
    if [ "$(uname -s)" = "Linux" ]; then
        sudo ldconfig 2>/dev/null || true
    fi
    success "libkrunfw removed"
else
    info "libkrunfw not found in ${LIB_DIR}"
fi

# Remove gvproxy
header "Removing gvproxy"
if [ -f "${BIN_DIR}/gvproxy" ]; then
    rm -f "${BIN_DIR}/gvproxy"
    success "gvproxy removed from ${BIN_DIR}"
else
    info "gvproxy not found in ${BIN_DIR}"
fi

header "Uninstall complete"
echo ""
echo "  Runtime dependencies have been removed."
echo "  The nanosb CLI binary was NOT removed."
echo ""
