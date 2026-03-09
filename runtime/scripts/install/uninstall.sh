#!/bin/bash
#
# Uninstall Nanosandbox runtime dependencies
#
# Removes all runtime dependencies installed by macos.sh, linux.sh, or gvproxy.sh.
# Useful for testing the install scripts from a clean state.
#
# Usage: ./scripts/install/uninstall.sh [--dry-run]
#
# Options:
#   --dry-run   Show what would be removed without actually removing anything
#

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

info()    { echo -e "${BLUE}[INFO]${NC} $1"; }
success() { echo -e "${GREEN}[OK]${NC} $1"; }
warn()    { echo -e "${YELLOW}[WARN]${NC} $1"; }
error()   { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

DRY_RUN=false
if [[ "$1" == "--dry-run" ]]; then
    DRY_RUN=true
    info "DRY RUN — no changes will be made"
    echo ""
fi

run_cmd() {
    if $DRY_RUN; then
        echo "  [dry-run] $*"
    else
        "$@"
    fi
}

OS="$(uname -s)"

echo ""
echo "========================================"
echo "  Nanosandbox Dependency Uninstaller"
echo "========================================"
echo ""

# =============================================================================
# gvproxy (cross-platform)
# =============================================================================

info "Checking gvproxy..."

gvproxy_removed=false
for gvproxy_path in \
    "$HOME/.local/bin/gvproxy" \
    "/usr/local/bin/gvproxy" \
    "/opt/homebrew/bin/gvproxy"; do
    if [[ -f "$gvproxy_path" ]]; then
        info "Removing gvproxy at $gvproxy_path"
        run_cmd rm -f "$gvproxy_path"
        gvproxy_removed=true
    fi
done

if $gvproxy_removed; then
    success "gvproxy removed"
else
    info "gvproxy not found — nothing to remove"
fi

# =============================================================================
# libkrun (platform-specific)
# =============================================================================

info "Checking libkrun..."

case "$OS" in
    Darwin)
        # macOS: uninstall via Homebrew
        if command -v brew &>/dev/null; then
            if brew list libkrun &>/dev/null 2>&1; then
                info "Uninstalling libkrun via Homebrew..."
                run_cmd brew uninstall libkrun
                success "libkrun uninstalled"

                # Also uninstall libkrunfw if it was pulled in as a dependency
                if brew list libkrunfw &>/dev/null 2>&1; then
                    info "Uninstalling libkrunfw (dependency)..."
                    run_cmd brew uninstall libkrunfw
                    success "libkrunfw uninstalled"
                fi
            else
                info "libkrun not installed via Homebrew — nothing to remove"
            fi
        else
            # Check if the file exists even without Homebrew
            if [[ -f "/opt/homebrew/lib/libkrun.dylib" ]]; then
                warn "libkrun.dylib found at /opt/homebrew/lib/ but Homebrew not available"
                warn "Remove manually: rm /opt/homebrew/lib/libkrun.dylib"
            else
                info "libkrun not found — nothing to remove"
            fi
        fi
        ;;

    Linux)
        # Linux: check package manager or manual install
        libkrun_removed=false

        # Check dnf (Fedora/RHEL)
        if command -v dnf &>/dev/null; then
            if rpm -q libkrun &>/dev/null 2>&1; then
                info "Uninstalling libkrun via dnf..."
                run_cmd sudo dnf remove -y libkrun
                libkrun_removed=true
                success "libkrun uninstalled via dnf"
            fi
        fi

        # Check apt (Debian/Ubuntu) — may have been built from source
        if ! $libkrun_removed; then
            for path in /usr/lib/libkrun.so /usr/lib64/libkrun.so /usr/local/lib/libkrun.so \
                        /usr/lib/x86_64-linux-gnu/libkrun.so /usr/lib/aarch64-linux-gnu/libkrun.so; do
                if [[ -f "$path" ]]; then
                    info "Removing libkrun at $path"
                    run_cmd sudo rm -f "$path"
                    libkrun_removed=true
                fi
            done
            if $libkrun_removed; then
                info "Updating library cache..."
                run_cmd sudo ldconfig
                success "libkrun removed"
            fi
        fi

        if ! $libkrun_removed; then
            info "libkrun not found — nothing to remove"
        fi
        ;;

    *)
        warn "Unsupported OS: $OS"
        ;;
esac

# =============================================================================
# Runtime artifacts (sockets, logs, SSH keys)
# =============================================================================

info "Cleaning runtime artifacts..."

artifact_count=0

# VM log files
for f in /tmp/nanosb-*-vm.log; do
    if [[ -f "$f" ]]; then
        run_cmd rm -f "$f"
        artifact_count=$((artifact_count + 1))
    fi
done

# gvproxy sockets
for f in /tmp/nanosb-*-vfkit.sock /tmp/nanosb-*-control.sock; do
    if [[ -S "$f" || -e "$f" ]]; then
        run_cmd rm -f "$f"
        artifact_count=$((artifact_count + 1))
    fi
done

# SSH key directories
for d in /tmp/nanosb-*-ssh; do
    if [[ -d "$d" ]]; then
        run_cmd rm -rf "$d"
        artifact_count=$((artifact_count + 1))
    fi
done

if [[ $artifact_count -gt 0 ]]; then
    success "Cleaned $artifact_count runtime artifact(s)"
else
    info "No runtime artifacts found"
fi

# =============================================================================
# Summary
# =============================================================================

echo ""
echo "========================================"
echo "  Uninstall Complete"
echo "========================================"
echo ""

if $DRY_RUN; then
    echo "This was a dry run. Re-run without --dry-run to actually remove."
else
    echo "All nanosandbox runtime dependencies have been removed."
    echo ""
    echo "To reinstall:"
    case "$OS" in
        Darwin) echo "  ./scripts/install/macos.sh" ;;
        Linux)  echo "  ./scripts/install/linux.sh" ;;
    esac
    echo ""
    echo "Verify with:"
    echo "  nanosb doctor"
fi
echo ""
