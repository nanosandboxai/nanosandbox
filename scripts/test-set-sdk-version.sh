#!/usr/bin/env bash
#
# test-set-sdk-version.sh — verifies set-sdk-version.sh writes and checks every
# SDK manifest in an isolated temp root (never touches the working tree).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/sdk/python" "$tmp/sdk/typescript" "$tmp/sdk/java" \
  "$tmp/sdk/ruby" "$tmp/sdk/dotnet/nanosandbox" "$tmp/sdk/kotlin" \
  "$tmp/sdk/go/nanosandbox" "$tmp/sdk/java/src/main/java/nanosandbox" \
  "$tmp/sdk/ruby/lib" "$tmp/sdk/python/nanosandbox" \
  "$tmp/sdk/typescript/nanosandbox"
cp "$ROOT/VERSION" "$tmp/VERSION"
cp "$ROOT/Cargo.toml" "$ROOT/Cargo.lock" "$tmp/"
cp "$ROOT/sdk/python/pyproject.toml" "$tmp/sdk/python/"
cp "$ROOT/sdk/typescript/package.json" "$tmp/sdk/typescript/"
cp "$ROOT/sdk/java/pom.xml" "$tmp/sdk/java/"
cp "$ROOT/sdk/ruby/nanosandbox.gemspec" "$tmp/sdk/ruby/"
cp "$ROOT/sdk/dotnet/nanosandbox/Nanosandbox.csproj" "$tmp/sdk/dotnet/nanosandbox/"
cp "$ROOT/sdk/kotlin/build.gradle.kts" "$tmp/sdk/kotlin/"
cp "$ROOT/sdk/ruby/lib/nanosandbox.rb" "$tmp/sdk/ruby/lib/"
cp "$ROOT/sdk/go/nanosandbox/runtime.go" "$tmp/sdk/go/nanosandbox/"
cp "$ROOT/sdk/java/src/main/java/nanosandbox/ManagedRuntime.java" "$tmp/sdk/java/src/main/java/nanosandbox/"
cp "$ROOT/sdk/python/nanosandbox/runtime.py" "$tmp/sdk/python/nanosandbox/"
cp "$ROOT/sdk/typescript/nanosandbox/runtime.mjs" "$tmp/sdk/typescript/nanosandbox/"
cp "$ROOT/sdk/dotnet/nanosandbox/ManagedRuntime.cs" "$tmp/sdk/dotnet/nanosandbox/"

fail=0
ok()  { printf 'ok   %s\n' "$1"; }
bad() { printf 'FAIL %s\n' "$1"; fail=1; }
has() { if grep -qE -- "$2" "$3"; then ok "$1"; else bad "$1 (no /$2/ in $3)"; fi; }

SDK_VERSION_ROOT="$tmp" "$ROOT/scripts/set-sdk-version.sh" 9.9.9 >/dev/null

[ "$(cat "$tmp/VERSION")" = "9.9.9" ] && ok "VERSION" || bad "VERSION"
has "cargo.toml"   '^version = "9\.9\.9"'                 "$tmp/Cargo.toml"
has "python"       '^version = "9\.9\.9"'                 "$tmp/sdk/python/pyproject.toml"
has "typescript"   '"version": "9\.9\.9"'                 "$tmp/sdk/typescript/package.json"
has "java"         '<version>9\.9\.9</version>'           "$tmp/sdk/java/pom.xml"
has "ruby"         'spec\.version[[:space:]]*= "9\.9\.9"' "$tmp/sdk/ruby/nanosandbox.gemspec"
has "dotnet"       '<Version>9\.9\.9</Version>'           "$tmp/sdk/dotnet/nanosandbox/Nanosandbox.csproj"
has "kotlin"       '^version = "9\.9\.9"'                 "$tmp/sdk/kotlin/build.gradle.kts"
has "ruby lib constant" '^  VERSION = "9\.9\.9"'          "$tmp/sdk/ruby/lib/nanosandbox.rb"
has "go constant"       '^const sdkVersion = "9\.9\.9"'   "$tmp/sdk/go/nanosandbox/runtime.go"
has "java constant"     'SDK_VERSION = "9\.9\.9"'         "$tmp/sdk/java/src/main/java/nanosandbox/ManagedRuntime.java"
has "python fallback"   '^        return "9\.9\.9"'        "$tmp/sdk/python/nanosandbox/runtime.py"
has "typescript fallback" '^  return "9\.9\.9";'          "$tmp/sdk/typescript/nanosandbox/runtime.mjs"
has "dotnet fallback"   'return v == null \? "9\.9\.9"'   "$tmp/sdk/dotnet/nanosandbox/ManagedRuntime.cs"
has "cargo.lock"   'name = "nanosandbox-sdk"'             "$tmp/Cargo.lock"
has "cargo.lock gateway" 'name = "gateway"'               "$tmp/Cargo.lock"

if SDK_VERSION_ROOT="$tmp" "$ROOT/scripts/set-sdk-version.sh" --check | grep -q 'version ok: 9.9.9'; then
  ok "check clean"
else
  bad "check clean"
fi

printf '1.2.3\n' > "$tmp/VERSION"
if SDK_VERSION_ROOT="$tmp" "$ROOT/scripts/set-sdk-version.sh" --check >/dev/null 2>&1; then
  bad "drift detected"
else
  ok "drift detected"
fi

[ "$fail" -eq 0 ] && echo "all set-sdk-version checks passed"
exit "$fail"
