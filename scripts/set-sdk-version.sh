#!/usr/bin/env bash
#
# set-sdk-version.sh — single source of truth for the SDK version.
#
# Writes <version> to the root VERSION file and propagates it to every language
# SDK manifest. With --check it only verifies that every manifest agrees with
# VERSION and exits non-zero on drift (a CI gate).
#
# Usage:
#   scripts/set-sdk-version.sh 0.3.0      # write VERSION + all manifests
#   scripts/set-sdk-version.sh --check    # fail on drift
#
# ROOT can be overridden with SDK_VERSION_ROOT (used by the test harness).
set -euo pipefail

ROOT="${SDK_VERSION_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
cd "$ROOT"

CARGO_TOML="Cargo.toml"
CARGO_LOCK="Cargo.lock"
PYPROJECT="sdk/python/pyproject.toml"
PKGJSON="sdk/typescript/package.json"
POM="sdk/java/pom.xml"
GEMSPEC="sdk/ruby/nanosandbox.gemspec"
CSPROJ="sdk/dotnet/nanosandbox/Nanosandbox.csproj"
GRADLE="sdk/kotlin/build.gradle.kts"

# Code-level version constants the SDKs read at runtime to pin the runtime they
# provision. Manifests alone are not enough: Ruby/Go/Java carry their own
# constant, and Python/TypeScript/.NET fall back to one when package metadata is
# unavailable, so all six must move with VERSION.
RUBY_LIB="sdk/ruby/lib/nanosandbox.rb"
GO_RUNTIME="sdk/go/nanosandbox/runtime.go"
JAVA_SRC="sdk/java/src/main/java/nanosandbox/ManagedRuntime.java"
PY_RUNTIME="sdk/python/nanosandbox/runtime.py"
TS_RUNTIME="sdk/typescript/nanosandbox/runtime.mjs"
CS_RUNTIME="sdk/dotnet/nanosandbox/ManagedRuntime.cs"

# Workspace crates whose Cargo.lock [[package]] version tracks the SDK version.
CARGO_PACKAGES=(
  nanosb-cli nanosandbox-sdk nanosb-protocol nanosb-client nanosb-host
  nanosb-conformance sandbox gateway runtime runtime-config libkrun-sys
)

read_version() {
  case "$1" in
    cargo-toml) awk '/^\[workspace\.package\]/{f=1} f&&/^version *=/{gsub(/[^0-9A-Za-z.-]/,"",$3); print $3; exit}' "$CARGO_TOML" ;;
    python)     awk -F'"' '/^version *=/{print $2; exit}' "$PYPROJECT" ;;
    typescript) awk -F'"' '/^  "version":/{print $4; exit}' "$PKGJSON" ;;
    java)       awk -F'[<>]' '/<artifactId>nanosandbox-sdk<\/artifactId>/{f=1} f&&/<version>/{print $3; exit}' "$POM" ;;
    ruby)       awk -F'"' '/spec\.version/{print $2; exit}' "$GEMSPEC" ;;
    dotnet)     awk -F'[<>]' '/<Version>/{print $3; exit}' "$CSPROJ" ;;
    kotlin)     awk -F'"' '/^version = /{print $2; exit}' "$GRADLE" ;;
    ruby-lib)   awk -F'"' '/^  VERSION = /{print $2; exit}' "$RUBY_LIB" ;;
    go)         awk -F'"' '/^const sdkVersion =/{print $2; exit}' "$GO_RUNTIME" ;;
    java-src)   awk -F'"' '/SDK_VERSION =/{print $2; exit}' "$JAVA_SRC" ;;
    python-src) awk -F'"' '/^        return "/{print $2; exit}' "$PY_RUNTIME" ;;
    typescript-src) awk -F'"' '/^  return "/{print $2; exit}' "$TS_RUNTIME" ;;
    dotnet-src) awk -F'"' '/return v == null \? "/{print $2; exit}' "$CS_RUNTIME" ;;
    *)          echo "unknown manifest: $1" >&2; return 2 ;;
  esac
}

read_lock_version() {
  awk -v p="$1" '$0=="[[package]]"{f=0} $0=="name = \""p"\""{f=1} f&&/^version =/{gsub(/[^0-9A-Za-z.-]/,"",$3); print $3; exit}' "$CARGO_LOCK"
}

write_all() {
  local v="$1"

  printf '%s\n' "$v" > VERSION

  perl -0pi -e 's/(\nversion = ")[^"]+(")/${1}'"$v"'${2}/' "$CARGO_TOML"
  perl -i -pe 's/^version = "[^"]+"/version = "'"$v"'"/' "$PYPROJECT"
  perl -i -pe 's/^  "version": "[^"]+"/  "version": "'"$v"'"/' "$PKGJSON"
  perl -0pi -e 's/(<artifactId>nanosandbox-sdk<\/artifactId>\s*\n\s*<version>)[^<]+(<\/version>)/${1}'"$v"'${2}/' "$POM"
  perl -i -pe 's/(spec\.version\s*= ")[^"]+(")/${1}'"$v"'${2}/' "$GEMSPEC"
  perl -i -pe 's/(<Version>)[^<]+(<\/Version>)/${1}'"$v"'${2}/' "$CSPROJ"
  perl -i -pe 's/^version = "[^"]+"/version = "'"$v"'"/' "$GRADLE"
  perl -i -pe 's/(^  VERSION = ")[^"]+(")/${1}'"$v"'${2}/' "$RUBY_LIB"
  perl -i -pe 's/(^const sdkVersion = ")[^"]+(")/${1}'"$v"'${2}/' "$GO_RUNTIME"
  perl -i -pe 's/(static final String SDK_VERSION = ")[^"]+(")/${1}'"$v"'${2}/' "$JAVA_SRC"
  perl -i -pe 's/(^        return ")[^"]+(")/${1}'"$v"'${2}/' "$PY_RUNTIME"
  perl -i -pe 's/(^  return ")[^"]+(";)/${1}'"$v"'${2}/' "$TS_RUNTIME"
  perl -i -pe 's/(return v == null \? ")[^"]+(" :)/${1}'"$v"'${2}/' "$CS_RUNTIME"

  local pkg
  for pkg in "${CARGO_PACKAGES[@]}"; do
    perl -0pi -e 's/(\[\[package\]\]\nname = "'"$pkg"'"\nversion = ")[^"]+(")/${1}'"$v"'${2}/' "$CARGO_LOCK"
  done
}

check_all() {
  local expected="$1" drift=0 got n
  for n in cargo-toml python typescript java ruby dotnet kotlin \
           ruby-lib go java-src python-src typescript-src dotnet-src; do
    got="$(read_version "$n")"
    if [ "$got" != "$expected" ]; then
      printf 'drift: %-12s %s (expected %s)\n' "$n" "${got:-<none>}" "$expected"
      drift=1
    fi
  done
  local pkg
  for pkg in "${CARGO_PACKAGES[@]}"; do
    got="$(read_lock_version "$pkg")"
    if [ "$got" != "$expected" ]; then
      printf 'drift: %-12s %s (expected %s)\n' "lock:$pkg" "${got:-<none>}" "$expected"
      drift=1
    fi
  done
  return "$drift"
}

case "${1:-}" in
  --check)
    expected="$(tr -d '[:space:]' < VERSION)"
    if check_all "$expected"; then
      echo "version ok: $expected"
    else
      exit 1
    fi
    ;;
  ""|-h|--help)
    echo "usage: scripts/set-sdk-version.sh <semver> | --check" >&2
    exit 2
    ;;
  *)
    v="$1"
    if ! printf '%s' "$v" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+'; then
      echo "error: '$v' is not a semver" >&2
      exit 2
    fi
    write_all "$v"
    echo "set SDK version to $v"
    ;;
esac
