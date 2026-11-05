#!/usr/bin/env bash
#
# set-version.sh — set every component's version from a single source of truth.
#
# The single source of truth is the VERSION file at the repo root. This script
# takes a semver argument, writes it to VERSION, and atomically rewrites the
# version in every component manifest that carries one:
#
#   - root Cargo.toml            [workspace.package] version
#   - Cargo.lock                 every workspace-member package entry
#   - sdk/python/pyproject.toml
#   - sdk/typescript/package.json
#   - sdk/java/pom.xml
#   - guest/crates/exec-agent/Cargo.toml
#
# sdk/rust uses `version.workspace = true` (implicit) and is verified, never
# hardcoded. sdk/go (versioned via git tags), sdk/ruby (no gemspec), sdk/kotlin
# (no gradle manifest) and sdk/dotnet (non-packable test project) carry no
# version field and are reported as skipped.
#
# Idempotent: running it twice with the same version is a no-op. Fails loudly
# if a required file or version pattern is missing.
#
# Usage:
#   scripts/set-version.sh <semver>     e.g. scripts/set-version.sh 0.3.0

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# --- validate the semver argument -------------------------------------------
if [ "$#" -ne 1 ]; then
  echo "error: usage: set-version.sh <semver>" >&2
  exit 1
fi
NEW_VERSION="$1"
if ! printf '%s' "$NEW_VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "error: '$NEW_VERSION' is not a valid semver (expected X.Y.Z)" >&2
  exit 1
fi

# --- rewrite a file via awk to a temp file, then atomically move it ---------
# Fails loudly if the file is missing or the marker is absent. The verify
# program re-reads the file and fails if the new version is not present at the
# expected location (which also covers an unexpected version-line format).
rewrite() {
  local file="$1" marker="$2" awkprog="$3" verifyprog="$4"
  [ -f "$file" ] || { echo "error: $file not found" >&2; exit 1; }
  grep -Fq "$marker" "$file" || { echo "error: pattern '$marker' not found in $file" >&2; exit 1; }
  awk -v new="$NEW_VERSION" "$awkprog" "$file" > "$file.tmp"
  mv "$file.tmp" "$file"
  if ! awk -v new="$NEW_VERSION" "$verifyprog" "$file"; then
    echo "error: failed to set $NEW_VERSION in $file" >&2
    exit 1
  fi
  echo "  updated $file"
}

# --- 1. VERSION file (single source of truth) -------------------------------
printf '%s\n' "$NEW_VERSION" > "$REPO_ROOT/VERSION"
echo "  updated $REPO_ROOT/VERSION"

# --- 2. root Cargo.toml [workspace.package] version -------------------------
rewrite "$REPO_ROOT/Cargo.toml" '[workspace.package]' '
  /^\[workspace\.package\]/ { in_ws=1; print; next }
  in_ws && /^\[/ { in_ws=0 }
  in_ws && /^version = / { sub(/^version = ".*"/, "version = \"" new "\""); }
  { print }
' '
  /^\[workspace\.package\]/ { in_ws=1 }
  in_ws && /^version = / { if ($0 !~ "\"" new "\"") bad=1; found=1 }
  END { exit (bad || !found) }
'

# --- 3. Cargo.lock workspace-member entries ---------------------------------
# Every workspace member that carries the workspace version must move together,
# otherwise the lockfile is stale (the exact bug this tooling fixes).
export LOCK_MEMBERS='nanosb-cli nanosandbox nanosb-protocol nanosb-client nanosb-host nanosb-conformance sandbox gateway runtime runtime-config libkrun-sys'
rewrite "$REPO_ROOT/Cargo.lock" 'name = "nanosb-cli"' '
  BEGIN { n=split(ENVIRON["LOCK_MEMBERS"], a, " "); for (i=1;i<=n;i++) members[a[i]]=1 }
  /^name = "/ {
    name=$0; sub(/^name = "/, "", name); sub(/".*/, "", name)
    want = (name in members) ? 1 : 0
    print; next
  }
  want && /^version = / { sub(/^version = ".*"/, "version = \"" new "\""); want=0 }
  { print }
' '
  BEGIN { n=split(ENVIRON["LOCK_MEMBERS"], a, " "); for (i=1;i<=n;i++) members[a[i]]=1 }
  /^name = "/ {
    name=$0; sub(/^name = "/, "", name); sub(/".*/, "", name)
    want = (name in members) ? 1 : 0
    next
  }
  want && /^version = / { seen=1; if ($0 !~ "\"" new "\"") bad=1; want=0 }
  END { exit (bad || !seen) }
'

# --- 4. sdk/python/pyproject.toml -------------------------------------------
rewrite "$REPO_ROOT/sdk/python/pyproject.toml" '[project]' '
  /^\[project\]/ { in_proj=1; print; next }
  in_proj && /^\[/ { in_proj=0 }
  in_proj && /^version = / { sub(/^version = ".*"/, "version = \"" new "\""); }
  { print }
' '
  /^\[project\]/ { in_proj=1 }
  in_proj && /^version = / { if ($0 !~ "\"" new "\"") bad=1; found=1 }
  END { exit (bad || !found) }
'

# --- 5. sdk/typescript/package.json -----------------------------------------
rewrite "$REPO_ROOT/sdk/typescript/package.json" '"version":' '
  /"version":/ { sub(/"version": ".*"/, "\"version\": \"" new "\""); }
  { print }
' '
  /"version":/ { if ($0 !~ "\"" new "\"") bad=1; found=1 }
  END { exit (bad || !found) }
'

# --- 6. sdk/java/pom.xml ----------------------------------------------------
rewrite "$REPO_ROOT/sdk/java/pom.xml" '<artifactId>nanosandbox-java</artifactId>' '
  /<artifactId>nanosandbox-java<\/artifactId>/ { found=1; print; next }
  found && /<version>/ { sub(/<version>.*<\/version>/, "<version>" new "</version>"); found=0 }
  { print }
' '
  /<artifactId>nanosandbox-java<\/artifactId>/ { found=1 }
  found && /<version>/ { seen=1; if ($0 !~ new) bad=1; found=0 }
  END { exit (bad || !seen) }
'

# --- 7. guest/crates/exec-agent/Cargo.toml ----------------------------------
rewrite "$REPO_ROOT/guest/crates/exec-agent/Cargo.toml" '[package]' '
  /^\[package\]/ { in_pkg=1; print; next }
  in_pkg && /^\[/ { in_pkg=0 }
  in_pkg && /^version = / { sub(/^version = ".*"/, "version = \"" new "\""); }
  { print }
' '
  /^\[package\]/ { in_pkg=1 }
  in_pkg && /^version = / { if ($0 !~ "\"" new "\"") bad=1; found=1 }
  END { exit (bad || !found) }
'

# --- 8. sdk/rust — implicit via version.workspace = true --------------------
RUST_SDK="$REPO_ROOT/sdk/rust/Cargo.toml"
[ -f "$RUST_SDK" ] || { echo "error: $RUST_SDK not found" >&2; exit 1; }
if grep -q '^version\.workspace = true' "$RUST_SDK"; then
  echo "  $RUST_SDK uses version.workspace = true (implicit, unchanged)"
elif grep -q '^version = ' "$RUST_SDK"; then
  echo "error: $RUST_SDK hardcodes a version; must use version.workspace = true" >&2
  exit 1
else
  echo "error: $RUST_SDK has no version field" >&2
  exit 1
fi

# --- 9. SDKs without a version field (reported, not rewritten) --------------
# sdk/go: versioned via git tags (go.mod carries no version).
# sdk/ruby: no gemspec / packaging manifest.
# sdk/kotlin: no gradle manifest (built via build.sh into a jar).
# sdk/dotnet: non-packable test project (IsPackable=false).
for sdk in go ruby kotlin dotnet; do
  echo "  sdk/$sdk: no version field (skipped)"
done

echo "==> all components aligned to $NEW_VERSION"