#!/usr/bin/env bash
# check-agent-image-deps.sh — deterministic, offline gate enforcing the
# agent-CLI version wiring contract:
#
#   registries/docker/package.json                       (Dependabot npm manifest)
#   <-> registries/docker/Dockerfile.*                   (npm install -g specs)
#   <-> .github/workflows/registry-docker-build.yml      (build-args)
#
# Dependabot's npm ecosystem only reads package.json; it does NOT see
# `npm install -g` inside Dockerfiles. The build workflow reads versions from
# package.json with `node -p` and passes them as build-args. If any of those
# three legs drifts, a Dependabot bump silently never reaches the built image.
# This gate fails the build when that happens.
#
# Usage: scripts/check-agent-image-deps.sh [DOCKER_DIR]
#   DOCKER_DIR  directory holding package.json + Dockerfile.* (default:
#               <repo>/registries/docker). Fixtures are passed here and are
#               still checked against the REAL workflow file.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DOCKER_DIR="${1:-registries/docker}"
WORKFLOW="$ROOT/.github/workflows/registry-docker-build.yml"

fail=0
pass() { printf 'ok   %s\n' "$1"; }
bad()  { printf 'FAIL %s\n' "$1"; fail=1; }

if [ ! -f "$DOCKER_DIR/package.json" ]; then
  echo "check-agent-image-deps: no package.json in $DOCKER_DIR" >&2
  exit 1
fi
if [ ! -f "$WORKFLOW" ]; then
  echo "check-agent-image-deps: no workflow at $WORKFLOW" >&2
  exit 1
fi

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

# package.json dependencies, one "pkg version" line each, in manifest order.
python3 - "$DOCKER_DIR/package.json" > "$tmpdir/pkg-versions" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
    data = json.load(f)
for pkg, ver in data.get("dependencies", {}).items():
    print(pkg, ver)
PY

awk '{ print $1 }' "$tmpdir/pkg-versions" > "$tmpdir/pkg-names"

# Collected state (mappings deduped by package).
: > "$tmpdir/mappings"   # pkg<TAB>VAR<TAB>version
: > "$tmpdir/installed"  # pkg names installed by some Dockerfile

for df in "$DOCKER_DIR"/Dockerfile.*; do
  [ -f "$df" ] || continue
  dfname="$(basename "$df")"

  while IFS= read -r line; do
    if ! printf '%s\n' "$line" | grep -qE 'npm[[:space:]]+(install|i|add)[[:space:]]+(-g|--global)([[:space:]]|$)'; then
      continue
    fi
    # Skip comment lines (first non-space char is '#').
    trimmed="${line#"${line%%[![:space:]]*}"}"
    case "$trimmed" in
      \#*) continue ;;
    esac

    # Only the arguments after the global flag are specs; other quoted strings
    # earlier on the same RUN (echo, cache dirs) are not packages. An unquoted
    # spec is a parse hole, not an escape hatch: fail instead of skipping it.
    args="$(printf '%s\n' "$line" | sed -nE 's/.*npm[[:space:]]+(install|i|add)[[:space:]]+(-g|--global)([[:space:]][^&|;\\]*).*/\3/p')"
    tokens="$(printf '%s\n' "$args" | grep -oE '"[^"]+"' | tr -d '"' || true)"
    if [ -z "$tokens" ]; then
      bad "p: $dfname: cannot parse a quoted npm spec in: $trimmed"
      continue
    fi
    while IFS= read -r token; do
      [ -n "$token" ] || continue

      # Resolve the token to (real_pkg, version_template).
      case "$token" in
        *npm:*)
          body="${token##*npm:}"
          real_pkg="${body%@*}"
          template="${body##*@}"
          ;;
        *@*)
          real_pkg="${token%@*}"
          template="${token##*@}"
          ;;
        *)
          real_pkg="$token"
          template=""
          ;;
      esac

      # (a) real_pkg must be a package.json dependency.
      if ! grep -qxF "$real_pkg" "$tmpdir/pkg-names"; then
        bad "a: $dfname: installs '$real_pkg' which is not in package.json"
        continue
      fi

      # (a2) the spec must be pinned through a ${*_VERSION} placeholder.
      var="$(printf '%s\n' "$template" | sed -nE 's/.*\$\{([A-Za-z0-9_]+_VERSION)\}.*/\1/p')"
      if [ -z "$var" ]; then
        bad "a2: $dfname: '$token' is not pinned via a *_VERSION build arg"
        continue
      fi

      # (c) an ARG <VAR>=<default> must exist in the same Dockerfile and
      #     <default> must equal package.json[real_pkg].
      expected="$(awk -v p="$real_pkg" '$1 == p { print $2 }' "$tmpdir/pkg-versions")"
      arg_line="$(grep -E "^[[:space:]]*ARG[[:space:]]+${var}=" "$df" | head -n1 || true)"
      if [ -z "$arg_line" ]; then
        bad "c: $dfname: no 'ARG $var=' for '$real_pkg'"
        continue
      fi
      default="${arg_line##*=}"
      default="${default%%[[:space:]]*}"
      if [ "$default" != "$expected" ]; then
        bad "c: $dfname: ARG $var=$default does not match package.json $real_pkg=$expected"
        continue
      fi

      # Record the mapping (deduped by package) for the ok output and (b)/(d).
      if ! awk -v p="$real_pkg" '$1 == p { found = 1 } END { exit !found }' "$tmpdir/mappings"; then
        printf '%s\t%s\t%s\n' "$real_pkg" "$var" "$expected" >> "$tmpdir/mappings"
      fi
      printf '%s\n' "$real_pkg" >> "$tmpdir/installed"
    done <<< "$tokens"
  done < "$df"
done

# ok lines, one per wired package, in package.json order.
while read -r pkg ver; do
  mapping="$(awk -v p="$pkg" '$1 == p { print }' "$tmpdir/mappings" | head -n1)"
  if [ -n "$mapping" ]; then
    var="$(printf '%s\n' "$mapping" | cut -f2)"
    pass "$pkg -> $var ($ver)"
  fi
done < "$tmpdir/pkg-versions"

# (b) every package.json dependency must be installed by some Dockerfile.
while read -r pkg ver; do
  if ! grep -qxF "$pkg" "$tmpdir/installed"; then
    bad "b: '$pkg' is pinned in package.json but no Dockerfile installs it"
  fi
done < "$tmpdir/pkg-versions"

# (d) every *_VERSION used by a Dockerfile must be a workflow build-arg wired
#     to the matching steps.versions output, resolved from the right package.
while IFS="$(printf '\t')" read -r pkg var version; do
  if ! grep -qE "^[[:space:]]*${var}=" "$WORKFLOW"; then
    bad "d: $var is used by a Dockerfile but is not passed as a build-arg"
    continue
  fi
  key="$(printf '%s' "$var" | sed -E 's/_VERSION$//' | tr '[:upper:]' '[:lower:]')"
  if ! grep -qF "${var}=\${{ steps.versions.outputs.${key} }}" "$WORKFLOW"; then
    bad "d: $var is not wired to steps.versions.outputs.${key}"
  fi
  if ! grep -F "echo \"${key}=" "$WORKFLOW" | grep -qF "$pkg"; then
    bad "d: the versions step does not resolve '$pkg' into output '$key'"
  fi
done < "$tmpdir/mappings"

total="$(wc -l < "$tmpdir/pkg-versions" | tr -d ' ')"
wired="$(wc -l < "$tmpdir/mappings" | tr -d ' ')"

if [ "$fail" -ne 0 ]; then
  echo "agent-image-deps: FAILED"
  exit 1
fi
echo "agent-image-deps: $wired/$total packages wired — gate passed"
