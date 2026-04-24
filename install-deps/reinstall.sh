#!/bin/bash
# Nanosandbox Runtime Dependencies Reinstaller
#
# Convenience wrapper: runs uninstall.sh then install.sh in one shot.
# Handy for "try again" flows after a failed or stale install.
#
# Usage (one-shot, immediate PATH activation in the current shell):
#   source <(curl -fsSL https://github.com/nanosandboxai/install-deps/releases/download/v0.2.0-rc6/reinstall.sh)
#
# Usage (classic pipe — new shells get PATH; current shell needs `source ~/.zshrc`):
#   curl -fsSL https://github.com/nanosandboxai/install-deps/releases/download/v0.2.0-rc6/reinstall.sh | bash
#
# Environment variables:
#   DEPS_VERSION     - Pin a specific version (default: latest)
#   NANOSANDBOX_HOME - Install prefix (default: ~/.nanosandbox)
#   PURGE_DATA=1     - Wipe ~/.nanosandbox/ fully (cache/bundles/sessions) without prompting
#   KEEP_DATA=1      - Keep ~/.nanosandbox/ user data without prompting
#   REINSTALL_BASE_URL - Override script source base URL (for testing unreleased builds)

set -euo pipefail

DEPS_VERSION="${DEPS_VERSION:-v0.2.0-rc6}"
BASE_URL="${REINSTALL_BASE_URL:-https://github.com/nanosandboxai/install-deps/releases/download/${DEPS_VERSION}}"

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

command -v curl >/dev/null 2>&1 || { error "curl is required"; exit 1; }

# Detect sourcing so we can export PATH back to the caller's shell. This must
# be checked at top-level so that when the script is sourced, the install.sh
# inner sourcing also bubbles PATH up through the same chain.
_sourced=0
if [ -n "${BASH_SOURCE[0]:-}" ] && [ "${BASH_SOURCE[0]}" != "${0}" ]; then
    _sourced=1
fi

echo "Nanosandbox Runtime Dependencies Reinstaller"
echo "============================================"
info "Version:  ${DEPS_VERSION}"
info "Source:   ${BASE_URL}"
[ "$_sourced" = "1" ] && info "Mode:     sourced (PATH will activate in this shell)" \
                     || info "Mode:     subshell (rc file will be updated; current shell unchanged)"

# ─── Fetch the two scripts to a tmpdir ───────────────────────────────────────

tmpdir="$(mktemp -d)"
trap 'rm -rf "${tmpdir:-}"' EXIT

header "Fetching scripts"
for script in uninstall.sh install.sh; do
    info "Downloading ${script}..."
    if ! curl -fsSL -o "${tmpdir}/${script}" "${BASE_URL}/${script}"; then
        error "Failed to download ${BASE_URL}/${script}"
        exit 1
    fi
done
success "Scripts ready in ${tmpdir}"

# ─── Stage 1: uninstall ──────────────────────────────────────────────────────

header "Stage 1: uninstall"
# uninstall.sh reads from /dev/tty for its y/N prompt unless PURGE_DATA or
# KEEP_DATA is set. It's always run in a subshell (bash), which is fine —
# removal side-effects don't need to be in the caller's shell.
bash "${tmpdir}/uninstall.sh"

# ─── Stage 2: install ────────────────────────────────────────────────────────

header "Stage 2: install"
if [ "$_sourced" = "1" ]; then
    # Source install.sh so its `export PATH=...` reaches the caller's shell.
    # shellcheck disable=SC1090
    source "${tmpdir}/install.sh"
else
    bash "${tmpdir}/install.sh"
fi

# ─── Summary ─────────────────────────────────────────────────────────────────

header "Reinstall complete"
if [ "$_sourced" = "1" ]; then
    success "Reinstalled ${DEPS_VERSION} and activated PATH in this shell"
    info "Try: nanosb doctor  (if the nanosb CLI is already installed)"
else
    info "Reinstalled ${DEPS_VERSION}."
    info "To activate in this terminal: source ~/.zshrc"
    info "Or re-run this reinstaller via: source <(curl -fsSL ${BASE_URL}/reinstall.sh)"
fi
