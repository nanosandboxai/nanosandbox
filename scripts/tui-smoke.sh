#!/bin/bash
#
# tui-smoke.sh — Nanosandbox TUI smoke-test harness
#
# Launches the nanosb TUI under a real PTY, sends a scripted key sequence
# (/help + Enter, /quit + Enter), and asserts that the help overlay appears.
#
# Usage:
#   ./scripts/tui-smoke.sh [--binary <path>]
#
# Environment variables:
#   NANOSB_BINARY    Path to nanosb binary (default: target/debug/nanosb)
#   SMOKE_BOOT_WAIT  Seconds to wait for TUI boot  (default: 3)
#   SMOKE_CMD_WAIT   Seconds to wait after a command (default: 2)
#   SMOKE_TIMEOUT    Total test timeout in seconds  (default: 30)
#
# Platform notes:
#   macOS: Uses `script -q <outfile> /dev/null` with a piped here-doc.
#          This creates a real PTY that crossterm recognises as a TTY.
#   Linux: `script -qec <cmd>` is the equivalent. If `expect` is available
#          it is preferred on both platforms for more reliable timing.
#
# Required runtime deps (must be installed before running):
#   - libkrun / libkrunfw (via install-deps or ~/.nanosandbox/libs/)
#   - gvproxy (via install-deps or ~/.nanosandbox/bin/gvproxy)
#   - Hypervisor.framework entitlement (macOS: codesign the binary)
#
# The script creates a temporary sandbox.yml, starts the TUI, sends
# keystrokes, captures output, and cleans up. It does NOT modify or
# delete ~/.nanosandbox state.
#
set -uo pipefail

# ─── Config ──────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Binary resolution: first arg --binary <path>, then NANOSB_BINARY, then default
NANOSB_BINARY="${NANOSB_BINARY:-}"
if [[ "$#" -ge 2 && "$1" == "--binary" ]]; then
    NANOSB_BINARY="$2"
    shift 2
fi
if [[ -z "$NANOSB_BINARY" ]]; then
    NANOSB_BINARY="$REPO_ROOT/target/debug/nanosb"
fi

SMOKE_BOOT_WAIT="${SMOKE_BOOT_WAIT:-3}"
SMOKE_CMD_WAIT="${SMOKE_CMD_WAIT:-2}"
SMOKE_TIMEOUT="${SMOKE_TIMEOUT:-30}"

# ─── Helpers ──────────────────────────────────────────────────────────────────

PASSED=0
FAILED=0

pass() {
    PASSED=$((PASSED + 1))
    printf '  [PASS] %s\n' "$1"
}

fail() {
    FAILED=$((FAILED + 1))
    printf '  [FAIL] %s\n' "$1" >&2
}

cleanup() {
    local exit_code=$?
    set +e

    # Kill any lingering nanosb process from this test
    if [[ -n "${NANOSB_PID:-}" ]]; then
        kill "$NANOSB_PID" 2>/dev/null || true
        wait "$NANOSB_PID" 2>/dev/null || true
    fi

    # Remove temp dir
    if [[ -n "${TMPDIR:-}" && -d "$TMPDIR" ]]; then
        rm -rf "$TMPDIR"
    fi

    # Print summary
    echo ""
    if [[ "$FAILED" -gt 0 ]]; then
        echo "FAIL: tui-smoke.sh — ${FAILED} failure(s), ${PASSED} passed"
    else
        echo "PASS: tui-smoke.sh — ${PASSED} passed"
    fi
    exit "$exit_code"
}

# ─── Pre-flight checks ───────────────────────────────────────────────────────

if [[ ! -x "$NANOSB_BINARY" ]]; then
    echo "Binary not found or not executable: $NANOSB_BINARY"
    echo "Building nanosb-cli..."
    cargo build -p nanosb-cli || {
        echo "FAIL: cargo build failed"
        exit 1
    }
    if [[ ! -x "$NANOSB_BINARY" ]]; then
        echo "FAIL: binary still not found after build: $NANOSB_BINARY"
        exit 1
    fi
fi

echo "Using binary: $NANOSB_BINARY"
echo "Boot wait: ${SMOKE_BOOT_WAIT}s  Cmd wait: ${SMOKE_CMD_WAIT}s  Timeout: ${SMOKE_TIMEOUT}s"
echo ""

# ─── Create an empty temp dir (welcome screen, no auto-started sandbox) ──────
#
# Running with NO sandbox.yml keeps the TUI on its welcome screen, so the
# harness deterministically exercises the PTY + command path without waiting
# on a microVM. Interactive panel attach is covered by the #[ignore] VM test
# (cargo test -p nanosb-cli tui::vm_test -- --ignored).

TMPDIR="$(mktemp -d /tmp/nanosb-tui-smoke.XXXXXX)"
trap cleanup EXIT INT TERM

echo "Using empty config dir: ${TMPDIR} (welcome screen)"
echo ""

# ─── Output capture file ─────────────────────────────────────────────────────

OUTFILE="$TMPDIR/tui-output.txt"
# We also capture stderr from the nanosb process (validation output, etc.)
ERRFILE="$TMPDIR/tui-stderr.txt"

# ─── Launch TUI under PTY ────────────────────────────────────────────────────
#
# Strategy: prefer `expect` (most reliable timing), fall back to `script`.
#
# On macOS: `script -q <outfile> /dev/null` creates a PTY and reads commands
# from stdin. We pipe a here-doc with sleep + keystrokes.
#
# On Linux: `script -qec <cmd>` is the equivalent.
#
# The key sequence:
#   1. Wait for TUI boot (SMOKE_BOOT_WAIT seconds)
#   2. Type "/help" + Enter
#   3. Wait for help overlay to render (SMOKE_CMD_WAIT seconds)
#   4. Type "/quit" + Enter
#   5. Wait for clean exit (SMOKE_CMD_WAIT seconds)

echo "Launching TUI under PTY..."

if command -v expect &>/dev/null; then
    # ── expect path (preferred) ──────────────────────────────────────────
    echo "  Using expect(1)"
    # Run with the empty temp dir as cwd (no sandbox.yml -> welcome screen).
    # The pty MUST have a sane size before the TUI starts, otherwise ratatui
    # draws a zero-area frame and nothing is painted.
    expect <<EXPECT_SCRIPT > "$OUTFILE" 2>"$ERRFILE" &
set timeout $SMOKE_TIMEOUT
set boot_wait $SMOKE_BOOT_WAIT
set cmd_wait $SMOKE_CMD_WAIT
set stty_init "rows 40 columns 120"

log_file -a "$OUTFILE"
cd "$TMPDIR"
spawn -noecho "$NANOSB_BINARY"

# Let the TUI finish prerequisite checks and paint the welcome screen.
sleep \$boot_wait

send "/help\r"
sleep \$cmd_wait
send "/quit\r"
sleep \$cmd_wait
expect eof
EXPECT_SCRIPT
    NANOSB_PID=$!

else
    # ── script path (fallback) ──────────────────────────────────────────
    echo "  Using script(1) — install expect(1) for more reliable timing"
    KEYSFILE="$TMPDIR/keys.txt"
    {
        sleep "$SMOKE_BOOT_WAIT"
        printf '/help\r'
        sleep "$SMOKE_CMD_WAIT"
        printf '/quit\r'
        sleep "$SMOKE_CMD_WAIT"
    } > "$KEYSFILE" &

    KEYS_PID=$!

    # Run from the empty temp dir (no sandbox.yml -> welcome screen) with an
    # explicit 40x120 pty size so the frame has real dimensions.
    if [[ "$(uname -s)" == "Darwin" ]]; then
        ( cd "$TMPDIR" && stty rows 40 columns 120; script -q "$OUTFILE" "$NANOSB_BINARY" ) < "$KEYSFILE" 2>"$ERRFILE" &
    else
        ( cd "$TMPDIR" && stty rows 40 columns 120; script -qec "$NANOSB_BINARY" "$OUTFILE" ) < "$KEYSFILE" 2>"$ERRFILE" &
    fi
    NANOSB_PID=$!

    wait "$KEYS_PID" 2>/dev/null || true
fi

# ─── Wait for TUI to exit (with overall timeout) ─────────────────────────────

echo "  Waiting for TUI to exit (timeout: ${SMOKE_TIMEOUT}s)..."
WAIT_START=$(date +%s)
while true; do
    if ! kill -0 "$NANOSB_PID" 2>/dev/null; then
        echo "  TUI exited cleanly."
        break
    fi
    NOW=$(date +%s)
    ELAPSED=$((NOW - WAIT_START))
    if [[ "$ELAPSED" -ge "$SMOKE_TIMEOUT" ]]; then
        echo "  WARN: TUI did not exit within ${SMOKE_TIMEOUT}s — killing..."
        kill "$NANOSB_PID" 2>/dev/null || true
        sleep 1
        break
    fi
    sleep 0.5
done

wait "$NANOSB_PID" 2>/dev/null || true
echo ""

# ─── Assertions ──────────────────────────────────────────────────────────────

echo "Checking output for help overlay marker..."
echo ""

# Read the captured output. The PTY transcript (OUTFILE) is the primary
# artifact; ERRFILE (nanosb's own stderr) is only a diagnostic fallback and
# must not be used to satisfy the marker assertion, so we keep them separate.
CAPTURED=""
if [[ -s "$OUTFILE" ]]; then
    CAPTURED="$(cat "$OUTFILE")"
fi

if [[ -z "$CAPTURED" ]]; then
    fail "No PTY output captured from TUI (outfile empty)"
    echo ""
    echo "--- stderr ---"
    cat "$ERRFILE" 2>/dev/null || echo "(empty)"
    echo "--- stdout ---"
    cat "$OUTFILE" 2>/dev/null || echo "(empty)"
    echo "---"
    exit 1
fi

# Assert: the /help overlay rendered (strong marker only — no generic fallback,
# otherwise the harness could pass without the TUI ever accepting a command).
if echo "$CAPTURED" | grep -q "Available commands"; then
    pass "Found 'Available commands' in TUI output"
else
    fail "Could not find the /help overlay marker 'Available commands' in TUI output"
    echo ""
    echo "--- First 80 lines of captured output ---"
    echo "$CAPTURED" | head -80
    echo "---"
    exit 1
fi

# ─── Done ────────────────────────────────────────────────────────────────────

echo ""
if [[ "$FAILED" -gt 0 ]]; then
    echo "FAIL: tui-smoke.sh"
    exit 1
else
    echo "PASS: tui-smoke.sh"
    exit 0
fi
