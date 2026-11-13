#!/usr/bin/env bash
# check-docs.sh — docs-accuracy gate.
#
# Two layers:
#   1. Surface-based checks via scripts/docs/check-docs-surface.py, which
#      validates fenced-block commands/flags, relative links, and the
#      Windows/WHPX/HCS platform guard against scripts/docs/cli-surface.json
#      (regenerated from src/main.rs + src/tui/commands.rs).
#   2. Grep-based spot checks on README + component docs (below).
#
# Usage: scripts/check-docs.sh [DOCS_DIR]
#   DOCS_DIR  optional markdown tree to check (default: docs). CI passes
#             test/check-docs/fixtures to assert the negative fixture fails.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DOCS_DIR="${1:-docs}"

fail=0
pass() { printf 'ok   %s\n' "$1"; }
bad()  { printf 'FAIL %s\n' "$1"; fail=1; }
has()  { if grep -qE -- "$2" "$3"; then pass "$1"; else bad "$1 (missing /$2/ in $3)"; fi; }
hasnt(){ if grep -qE -- "$2" "$3"; then bad "$1 (found /$2/ in $3)"; else pass "$1"; fi; }

# --- regenerate the CLI/TUI surface from source ---------------------------
python3 scripts/docs/extract-cli-surface.py

# --- surface-based accuracy checks ----------------------------------------
if ! python3 scripts/docs/check-docs-surface.py --docs-dir "$DOCS_DIR"; then
  fail=1
fi

# --- grep-based spot checks (README + component docs) ---------------------
has   "README: Linux/KVM support documented"      "KVM" README.md
has   "README: links Linux live verification"     "live-verification-linux" README.md
hasnt "README: no stale 'Linux not supported'"    "Linux is not yet supported" README.md

for c in apply restart describe logs attach sessions projects projects-forget gc; do
  has "README: CLI lists '$c'" "nanosb $c" README.md
done

has "README: --resume flag"       "--resume" README.md
has "README: --session flag"      "--session" README.md
has "README: NANOSANDBOX_HOME"    "NANOSANDBOX_HOME" README.md
has "README: SDK section"         "Language SDKs" README.md
has "README: advanced surface"    "Advanced sandbox surface" README.md
has "README: packaging section"   "package-sdk.sh" README.md

has   "install-deps README: libs path"        "nanosandbox/libs" install-deps/README.md
has   "install-deps README: bin path"         "nanosandbox/bin" install-deps/README.md
has   "install-deps README: doctor snippet"   "nanosb doctor" install-deps/README.md
hasnt "install-deps README: no /usr/local/lib" "/usr/local/lib" install-deps/README.md
hasnt "install-deps README: no ~/.local/bin"   "\.local/bin" install-deps/README.md

has   "AGENTS.md: docs-sync policy"     "Documentation is part of the change" AGENTS.md
has   "AGENTS.md: version SOT convention" "version.workspace" AGENTS.md
has   "AGENTS.md: host layer crate"     "crates/host" AGENTS.md
has   "README: host layer crate"        "crates/host" README.md
hasfile() { if [ -f "$1" ]; then pass "$1 exists"; else bad "$1 missing"; fi; }
hasfile crates/host/Cargo.toml
hasfile crates/host/src/spec.rs
hasfile crates/host/src/deploy.rs

hasnt "runtime README: no 'in development'" "in development" runtime/README.md
hasnt "runtime README: no 'separate repo'"  "separate repo" runtime/README.md
hasnt "runtime README: no start_next"       "start_next" runtime/README.md
has   "sandbox README: mentions Kotlin"     "Kotlin" sandbox/README.md
hasnt "agents-registry README: no devdone-labs" "devdone-labs" agents-registry/README.md

if [ "$fail" -ne 0 ]; then
  echo "docs check FAILED"
  exit 1
fi
echo "docs check passed"