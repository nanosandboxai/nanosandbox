#!/usr/bin/env bash
#
# test-set-version.sh — verify set-version.sh aligns every component.
#
# Copies the version-bearing manifests into a temp dir, runs set-version.sh
# 9.9.9 against that copy, and asserts every file now reads 9.9.9. Reports
# pass/fail and exits non-zero on any failure.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# --- build a minimal repo skeleton in $TMP ----------------------------------
mkdir -p "$TMP/scripts" \
         "$TMP/sdk/python" "$TMP/sdk/typescript" "$TMP/sdk/java" "$TMP/sdk/rust" \
         "$TMP/guest/crates/exec-agent"

cp "$REPO_ROOT/scripts/set-version.sh" "$TMP/scripts/set-version.sh"
cp "$REPO_ROOT/Cargo.toml"              "$TMP/Cargo.toml"
cp "$REPO_ROOT/Cargo.lock"              "$TMP/Cargo.lock"
cp "$REPO_ROOT/sdk/python/pyproject.toml"          "$TMP/sdk/python/pyproject.toml"
cp "$REPO_ROOT/sdk/typescript/package.json"        "$TMP/sdk/typescript/package.json"
cp "$REPO_ROOT/sdk/java/pom.xml"                   "$TMP/sdk/java/pom.xml"
cp "$REPO_ROOT/sdk/rust/Cargo.toml"                "$TMP/sdk/rust/Cargo.toml"
cp "$REPO_ROOT/guest/crates/exec-agent/Cargo.toml" "$TMP/guest/crates/exec-agent/Cargo.toml"

# --- run set-version.sh against the temp copy -------------------------------
"$TMP/scripts/set-version.sh" 9.9.9

# --- assert every file reads 9.9.9 ------------------------------------------
fail=0
check() {
  local desc="$1"; shift
  if "$@"; then
    echo "PASS: $desc"
  else
    echo "FAIL: $desc"
    fail=1
  fi
}

check "VERSION is 9.9.9" \
  grep -qx '9.9.9' "$TMP/VERSION"

check "Cargo.toml [workspace.package] version is 9.9.9" \
  awk '/^\[workspace\.package\]/{inws=1} inws && /^version = /{found=1; exit !($0 ~ /"9.9.9"/)} END{exit !found}' "$TMP/Cargo.toml"

check "Cargo.lock workspace members are 9.9.9" \
  awk '
    /^name = "nanosb-cli"/ || /^name = "nanosandbox"/ || /^name = "nanosb-protocol"/ || \
    /^name = "nanosb-client"/ || /^name = "nanosb-host"/ || /^name = "nanosb-conformance"/ || \
    /^name = "sandbox"/ || /^name = "gateway"/ || /^name = "runtime"/ || \
    /^name = "runtime-config"/ || /^name = "libkrun-sys"/ {
      want=1; next
    }
    want && /^version = / { if ($0 !~ /"9.9.9"/) bad=1; want=0 }
    END { exit bad }
  ' "$TMP/Cargo.lock"

check "sdk/python/pyproject.toml version is 9.9.9" \
  awk '/^\[project\]/{inp=1} inp && /^version = /{found=1; exit !($0 ~ /"9.9.9"/)} END{exit !found}' "$TMP/sdk/python/pyproject.toml"

check "sdk/typescript/package.json version is 9.9.9" \
  grep -q '"version": "9.9.9"' "$TMP/sdk/typescript/package.json"

check "sdk/java/pom.xml version is 9.9.9" \
  awk '/<artifactId>nanosandbox-java<\/artifactId>/{found=1; getline; exit !($0 ~ /<version>9.9.9<\/version>/)} END{exit !found}' "$TMP/sdk/java/pom.xml"

check "guest/crates/exec-agent/Cargo.toml version is 9.9.9" \
  awk '/^\[package\]/{inp=1} inp && /^version = /{found=1; exit !($0 ~ /"9.9.9"/)} END{exit !found}' "$TMP/guest/crates/exec-agent/Cargo.toml"

check "sdk/rust/Cargo.toml still uses version.workspace = true" \
  grep -q '^version\.workspace = true' "$TMP/sdk/rust/Cargo.toml"

if [ "$fail" -eq 0 ]; then
  echo "==> PASS: all components aligned to 9.9.9"
  exit 0
else
  echo "==> FAIL: one or more components not aligned" >&2
  exit 1
fi