#!/bin/bash
# Nanosandbox Runtime Dependencies Uninstaller
#
# Removes: libkrunfw + gvproxy from ~/.nanosandbox/
# Optionally removes all nanosandbox data (cache, bundles, sessions).
# Does NOT remove the nanosb CLI binary.
#
# Usage:
#   curl -fsSL https://github.com/nanosandboxai/install-deps/releases/latest/download/uninstall.sh | bash
#
# Environment variables:
#   NANOSANDBOX_HOME - Base directory (default: ~/.nanosandbox)
#   PURGE_DATA       - Set to 1 to skip prompt and wipe ~/.nanosandbox/ entirely

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

if [ -d "$LIB_DIR" ]; then
    rm -rf "$LIB_DIR"
    success "Removed $LIB_DIR"
    removed=true
fi

# Clean up legacy location
shopt -s nullglob
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

# Remove /usr/local/bin symlink if it points to our gvproxy.
# We only remove symlinks (not real binaries) and only ones we own.
sym="/usr/local/bin/gvproxy"
if [ -L "$sym" ]; then
    target="$(readlink "$sym" 2>/dev/null || true)"
    case "$target" in
        "${BIN_DIR}/gvproxy"|"${LEGACY_BIN_DIR}/gvproxy")
            sudo rm -f "$sym" 2>/dev/null || rm -f "$sym" 2>/dev/null || true
            success "Removed symlink $sym"
            gvproxy_removed=true
            ;;
    esac
fi

if ! $gvproxy_removed; then
    info "gvproxy not found"
fi

# ─── Clean PATH from shell rc files ──────────────────────────────────────────

header "Cleaning PATH"
path_cleaned=false
for rc in "$HOME/.zshrc" "$HOME/.bashrc" "$HOME/.bash_profile"; do
    [ -f "$rc" ] || continue
    if grep -qF "$BIN_DIR" "$rc" 2>/dev/null; then
        # Remove the "# Added by Nanosandbox installer" comment and the next line
        # (the export PATH line referencing BIN_DIR).
        tmp="$(mktemp)"
        awk -v bin="$BIN_DIR" '
            /^# Added by Nanosandbox installer$/ { skip=2; next }
            skip>0 { skip--; next }
            index($0, bin)==0 { print }
        ' "$rc" > "$tmp" && mv "$tmp" "$rc"
        success "Removed PATH entry from $(basename "$rc")"
        path_cleaned=true
    fi
done
$path_cleaned || info "No PATH entry found in shell rc files"

# ─── Optionally remove all nanosandbox data ──────────────────────────────────

header "Cleaning caches"
if [ -d "$NANOSANDBOX_HOME" ]; then
    # Prompt from the controlling terminal so we still ask even when invoked
    # via `curl ... | bash` (which consumes stdin). PURGE_DATA=1 skips the
    # prompt; KEEP_DATA=1 keeps data without prompting.
    if [ "${PURGE_DATA:-0}" = "1" ]; then
        answer="y"
    elif [ "${KEEP_DATA:-0}" = "1" ]; then
        answer="n"
    elif [ -r /dev/tty ]; then
        printf '  Remove all nanosandbox data at %s? [y/N] ' "$NANOSANDBOX_HOME" > /dev/tty
        read -r answer < /dev/tty || answer=""
    else
        info "No controlling terminal — kept $NANOSANDBOX_HOME (set PURGE_DATA=1 to wipe)"
        answer=""
    fi

    purged=false
    case "$answer" in
        [yY]|[yY][eE][sS])
            rm -rf "$NANOSANDBOX_HOME"
            success "Removed $NANOSANDBOX_HOME"
            purged=true
            ;;
        *)
            [ -n "$answer" ] && info "Kept $NANOSANDBOX_HOME"
            # If the dir is now empty (no user data, just empty bin/), remove it.
            if [ -d "$NANOSANDBOX_HOME" ] && [ -z "$(ls -A "$NANOSANDBOX_HOME" 2>/dev/null)" ]; then
                rmdir "$NANOSANDBOX_HOME" 2>/dev/null && success "Removed empty $NANOSANDBOX_HOME"
            fi
            ;;
    esac
fi

# ─── Summary ─────────────────────────────────────────────────────────────────

header "Uninstall complete"
if [ "${purged:-false}" = "true" ]; then
    cat <<EOF
  All nanosandbox data was removed (including any nanosb CLI binary
  installed under ${NANOSANDBOX_HOME}/bin/).
EOF
else
    cat <<EOF
  Runtime dependencies have been removed.
  The nanosb CLI binary was NOT removed (kept under ${NANOSANDBOX_HOME}/bin/
  along with the rest of your nanosandbox data).
EOF
fi
