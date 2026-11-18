#!/usr/bin/env bash
# test/test_installer.sh - Installer unit tests: codesign rule + prerelease latest
#
# Runs scripts/install.sh and scripts/package-install.sh inside a sandbox:
# temp HOME + mocked curl/codesign/uname/sudo on a PATH prefix. No network,
# no real sudo, and no code is actually re-signed.
#
# Proves three installer behaviors:
#   1. a binary already validly signed with the hypervisor entitlement is NOT
#      re-signed (a Developer-ID signed + notarized release binary is left alone)
#   2. an unsigned / un-entitled binary IS ad-hoc signed WITH the entitlement
#      (local/dev builds keep working)
#   3. NANOSB_VERSION=latest resolves to a concrete prerelease tag and downloads
#      from /releases/download/<tag>/ (GitHub's /releases/latest/ 404s when the
#      newest release is a prerelease), incl. the linux-arm64 asset mapping
#
# lib.sh is intentionally NOT sourced: it requires a real nanosb binary in PATH.

set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
INSTALLER="$ROOT/scripts/install.sh"
PKG_INSTALLER="$ROOT/scripts/package-install.sh"

# ── Minimal harness (mirrors test/lib.sh style, standalone) ──────────────────
TESTS_RUN=0
TESTS_FAILED=0
CURRENT=""
begin() { CURRENT="$1"; TESTS_RUN=$((TESTS_RUN + 1)); printf '  RUN  %s\n' "$CURRENT"; }
ok()    { printf '  PASS %s\n' "$CURRENT"; }
fail()  { printf '  FAIL %s\n        %s\n' "$CURRENT" "$1"; TESTS_FAILED=$((TESTS_FAILED + 1)); }

GREEN='\033[0;32m'; RED='\033[0;31m'; BLUE='\033[0;34m'; BOLD='\033[1m'; RESET='\033[0m'

# ── Per-case sandbox: temp HOME + mock binaries on a PATH prefix ─────────────
new_sandbox() {
  SANDBOX="$(mktemp -d)"
  MOCKBIN="$SANDBOX/mockbin"
  HOME_DIR="$SANDBOX/home"
  NSB_HOME="$SANDBOX/nanosandbox"
  URL_LOG="$SANDBOX/urls.log"
  CODESIGN_LOG="$SANDBOX/codesign.log"
  SUDO_LOG="$SANDBOX/sudo.log"
  OUTLOG="$SANDBOX/install.out"
  mkdir -p "$MOCKBIN" "$HOME_DIR" "$NSB_HOME" "$SANDBOX/cwd"

  # Fake local "binary" used with NANOSB_BINARY=... (skips the binary download)
  printf '#!/bin/bash\nprintf "nanosb 0.0.0-local\n"\n' > "$SANDBOX/local-nanosb"
  chmod +x "$SANDBOX/local-nanosb"

  # uname: platform faked via MOCK_OS / MOCK_ARCH
  cat > "$MOCKBIN/uname" << 'EOF'
#!/bin/bash
for a in "$@"; do
  [ "$a" = "-s" ] && printf '%s\n' "${MOCK_OS:-Darwin}"
  [ "$a" = "-m" ] && printf '%s\n' "${MOCK_ARCH:-arm64}"
done
exit 0
EOF
  chmod +x "$MOCKBIN/uname"

  # curl: logs the URL, serves canned responses (prerelease tag / deps script).
  # The releases API returns a prerelease tag to exercise the prerelease path.
  cat > "$MOCKBIN/curl" << 'EOF'
#!/bin/bash
url=""; out=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    -H) shift 2 ;;                    # skip Accept header + value
    -f|-fsSL|-fL|--fail|--silent|--show-error|--location|-s|-S|-L)
        shift ;;
    *) url="$1"; shift ;;
  esac
done
printf '%s\n' "$url" >> "$URL_LOG"
if [ -n "$url" ] && printf '%s' "$url" | grep -q '^https://api.github.com'; then
  body='[{"tag_name": "v0.9.0-rc.1", "prerelease": true}]'
elif [ -n "$url" ] && printf '%s' "$url" | grep -q 'install-deps.sh'; then
  body=$'#!/bin/bash\n# mock deps installer\nexit 0\n'
else
  body='mock-binary-content'
fi
if [ -n "$out" ]; then printf '%s' "$body" > "$out"; else printf '%s\n' "$body"; fi
exit 0
EOF
  chmod +x "$MOCKBIN/curl"

  # codesign: MOCK_CODESIGN_MODE=valid  → signed + entitled (verify passes)
  #            MOCK_CODESIGN_MODE=invalid → unsigned / un-entitled (verify fails)
  # Re-sign calls (--force) exit 0 and append the entitlement plist they were
  # asked to apply, so tests can assert the entitlement is included.
  cat > "$MOCKBIN/codesign" << 'EOF'
#!/bin/bash
printf '%s\n' "$*" >> "$CODESIGN_LOG"
case "$*" in
  *--force*)
    entplist="$(printf '%s' "$*" | sed -E 's/.*--entitlements ([^ ]+) .*/\1/')"
    [ -n "$entplist" ] && [ -f "$entplist" ] && cat "$entplist" >> "$CODESIGN_LOG"
    exit 0 ;;
  *"--entitlements :-"*)
    if [ "${MOCK_CODESIGN_MODE:-valid}" = "valid" ]; then
      printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>com.apple.security.hypervisor</key><true/></dict></plist>'
      exit 0
    fi
    exit 1 ;;
  *--verify*)
    [ "${MOCK_CODESIGN_MODE:-valid}" = "valid" ] && exit 0 || exit 1 ;;
esac
exit 0
EOF
  chmod +x "$MOCKBIN/codesign"

  # sudo: no-op so no real sudo prompt / symlink ever happens
  cat > "$MOCKBIN/sudo" << 'EOF'
#!/bin/bash
printf 'sudo %s\n' "$*" >> "$SUDO_LOG"
exit 0
EOF
  chmod +x "$MOCKBIN/sudo"

  # nanosb: guard so a host-installed nanosb is never invoked
  cat > "$MOCKBIN/nanosb" << 'EOF'
#!/bin/bash
printf '%s\n' "nanosb 9.9.9-mock"
exit 0
EOF
  chmod +x "$MOCKBIN/nanosb"
}

run_installer() { # $1 = codesign mock mode, $2 = NANOSB_BINARY ("" = download)
  cd "$SANDBOX/cwd"
  MOCK_CODESIGN_MODE="$1" \
  MOCK_OS="${MOCK_OS:-Darwin}" MOCK_ARCH="${MOCK_ARCH:-arm64}" \
  URL_LOG="$URL_LOG" CODESIGN_LOG="$CODESIGN_LOG" SUDO_LOG="$SUDO_LOG" \
  HOME="$HOME_DIR" NANOSANDBOX_HOME="$NSB_HOME" \
  NANOSB_VERSION="latest" DEPS_VERSION="latest" \
  NANOSB_BINARY="$2" \
  PATH="$MOCKBIN:$PATH" bash "$INSTALLER" > "$OUTLOG" 2>&1
  RUN_RC=$?
  cd - >/dev/null 2>&1 || true
  return "$RUN_RC"
}

print_suite_header() {
  echo ""
  echo -e "${BOLD}━━━ $1 ━━━${RESET}"
}

print_summary() {
  echo ""
  echo -e "${BOLD}──────────────────────────────────${RESET}"
  echo -e "  Total:   $TESTS_RUN"
  if [[ $TESTS_FAILED -gt 0 ]]; then
    echo -e "  ${RED}Failed:  $TESTS_FAILED${RESET}"
    echo -e "  ${RED}${BOLD}SOME TESTS FAILED${RESET}"
    return 1
  else
    echo -e "  Failed:  0"
    echo -e "  ${GREEN}${BOLD}ALL TESTS PASSED${RESET}"
    return 0
  fi
}

print_suite_header "Installer scripts (codesign + prerelease latest)"

# ═══════════════════════════════════════════════════════════════════════════
# 1. Already validly signed WITH the hypervisor entitlement → NO re-sign
# ═══════════════════════════════════════════════════════════════════════════
begin "already signed + entitled binary is NOT re-signed"
new_sandbox
run_installer "valid" "$SANDBOX/local-nanosb"
rc=$?
if [ $rc -ne 0 ]; then
  fail "install.sh exited $rc"
elif grep -q -- '--force' "$CODESIGN_LOG"; then
  fail "found an ad-hoc re-sign (--force) over a validly signed binary: $(grep -- '--force' "$CODESIGN_LOG" | head -1)"
elif ! grep -q -- "--verify --deep --strict" "$CODESIGN_LOG"; then
  fail "signature was not even validated (no codesign --verify --deep --strict)"
elif ! grep -q 'skipping re-sign' "$OUTLOG"; then
  fail "expected 'skipping re-sign' message in installer output"
else
  ok
fi

# ═══════════════════════════════════════════════════════════════════════════
# 2. Unsigned / un-entitled binary IS ad-hoc signed WITH the entitlement
# ═══════════════════════════════════════════════════════════════════════════
begin "unsigned binary IS ad-hoc signed with the hypervisor entitlement"
new_sandbox
run_installer "invalid" "$SANDBOX/local-nanosb"
rc=$?
if [ $rc -ne 0 ]; then
  fail "install.sh exited $rc"
elif ! grep -q -- "--force" "$CODESIGN_LOG"; then
  fail "expected an ad-hoc re-sign (--force) for the unsigned binary; codesign log: $(cat "$CODESIGN_LOG")"
elif ! grep -q 'com.apple.security.hypervisor' "$CODESIGN_LOG"; then
  fail "re-sign did not apply the hypervisor entitlement plist; codesign log: $(cat "$CODESIGN_LOG")"
elif ! grep -q 'Signed with com.apple.security.hypervisor' "$OUTLOG"; then
  fail "expected 'Signed with com.apple.security.hypervisor' message in installer output"
else
  ok
fi

# ═══════════════════════════════════════════════════════════════════════════
# 3. latest → tagged (prerelease) URL + linux-arm64 asset mapping
#    (mock codesign mode irrelevant on Linux; force the download path)
# ═══════════════════════════════════════════════════════════════════════════
begin "latest resolves to tagged URL (prerelease) incl. linux-arm64 asset"
new_sandbox
MOCK_OS="Linux" MOCK_ARCH="aarch64" run_installer "invalid" ""
rc=$?
if [ $rc -ne 0 ]; then
  fail "install.sh exited $rc"
elif grep -q 'releases/latest/download' "$URL_LOG"; then
  fail "still using /releases/latest/download (404s on prereleases): $(cat "$URL_LOG")"
elif ! grep -q 'releases/download/v0.9.0-rc.1/nanosb-linux-arm64' "$URL_LOG"; then
  fail "no tagged linux-arm64 URL; urls: $(cat "$URL_LOG")"
elif ! grep -q 'releases/download/v0.9.0-rc.1/agent_defaults.yaml' "$URL_LOG"; then
  fail "no tagged agent_defaults.yaml URL; urls: $(cat "$URL_LOG")"
elif ! grep -q 'releases/download/v0.9.0-rc.1/exec-agent-aarch64' "$URL_LOG"; then
  fail "no tagged exec-agent URL; urls: $(cat "$URL_LOG")"
elif grep -q 'Could not resolve latest release tag' "$OUTLOG"; then
  fail "latest tag did not resolve; output: $(cat "$OUTLOG")"
else
  ok
fi

# ═══════════════════════════════════════════════════════════════════════════
# 4. package-install.sh applies the same codesign rule
# ═══════════════════════════════════════════════════════════════════════════
begin "package-install.sh leaves a signed+entitled helper alone"
new_sandbox
BUNDLE="$SANDBOX/bundle"
mkdir -p "$BUNDLE/bin" "$BUNDLE/libs" "$BUNDLE/guest-bin"
printf '#!/bin/bash\nprintf "runtime 0.0.0\n"\n' > "$BUNDLE/bin/nanosb-runtime"
chmod +x "$BUNDLE/bin/nanosb-runtime"
printf 'mock libkrunfw dylib\n' > "$BUNDLE/libs/libkrunfw.5.dylib"
cp "$PKG_INSTALLER" "$BUNDLE/install.sh"
chmod +x "$BUNDLE/install.sh"
(
  cd "$SANDBOX"
  MOCK_CODESIGN_MODE="valid" \
  URL_LOG="$URL_LOG" CODESIGN_LOG="$CODESIGN_LOG" SUDO_LOG="$SUDO_LOG" \
  HOME="$HOME_DIR" NANOSB_HOME="$NSB_HOME" \
  PATH="$MOCKBIN:$PATH" bash "$BUNDLE/install.sh" > "$OUTLOG" 2>&1
)
if grep -q -- "--force" "$CODESIGN_LOG"; then
  fail "package-install re-signed a signed+entitled helper: $(cat "$CODESIGN_LOG")"
else
  ok
fi

begin "package-install.sh ad-hoc re-signs an unsigned helper WITH the entitlement"
new_sandbox
BUNDLE="$SANDBOX/bundle"
mkdir -p "$BUNDLE/bin" "$BUNDLE/libs" "$BUNDLE/guest-bin"
printf '#!/bin/bash\nprintf "runtime 0.0.0\n"\n' > "$BUNDLE/bin/nanosb-runtime"
chmod +x "$BUNDLE/bin/nanosb-runtime"
printf 'mock libkrunfw dylib\n' > "$BUNDLE/libs/libkrunfw.5.dylib"
cp "$PKG_INSTALLER" "$BUNDLE/install.sh"
chmod +x "$BUNDLE/install.sh"
(
  cd "$SANDBOX"
  MOCK_CODESIGN_MODE="invalid" \
  URL_LOG="$URL_LOG" CODESIGN_LOG="$CODESIGN_LOG" SUDO_LOG="$SUDO_LOG" \
  HOME="$HOME_DIR" NANOSB_HOME="$NSB_HOME" \
  PATH="$MOCKBIN:$PATH" bash "$BUNDLE/install.sh" > "$OUTLOG" 2>&1
)
if ! grep -q -- "--force --sign - --entitlements" "$CODESIGN_LOG"; then
  fail "expected an entitled ad-hoc re-sign; codesign log: $(cat "$CODESIGN_LOG")"
elif ! grep -q 'com.apple.security.hypervisor' "$CODESIGN_LOG"; then
  fail "re-sign did not apply the hypervisor entitlement; codesign log: $(cat "$CODESIGN_LOG")"
else
  ok
fi

print_summary
