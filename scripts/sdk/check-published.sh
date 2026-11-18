#!/usr/bin/env bash
#
# check-published.sh <sdk> <version>
#
# Exit 0 if the SDK's artifact at <version> is already present in its registry
# (so the release pipeline can skip it idempotently); exit 1 if it is not yet
# published; exit 2 on usage error.
set -euo pipefail

sdk="${1:?usage: check-published.sh <sdk> <version>}"
ver="${2:?usage: check-published.sh <sdk> <version>}"
UA="nanosandbox-release-check"

url=""
pattern=""
case "$sdk" in
  rust)            url="https://crates.io/api/v1/crates/nanosandbox-sdk/$ver" ;;
  python)          url="https://pypi.org/pypi/nanosandbox-sdk/$ver/json" ;;
  typescript|npm)  url="https://registry.npmjs.org/nanosandbox-sdk/$ver" ;;
  ruby)            url="https://rubygems.org/api/v2/rubygems/nanosandbox-sdk/versions/$ver.json" ;;
  dotnet)          url="https://api.nuget.org/v3-flatcontainer/nanosandbox.sdk/$ver/nanosandbox.sdk.$ver.nupkg" ;;
  java)            url="https://repo1.maven.org/maven2/ai/nanosandbox/nanosandbox-sdk/$ver/nanosandbox-sdk-$ver.pom" ;;
  kotlin)          url="https://repo1.maven.org/maven2/ai/nanosandbox/nanosandbox-sdk-kotlin/$ver/nanosandbox-sdk-kotlin-$ver.pom" ;;
  go)              url="https://proxy.golang.org/github.com/nanosandboxai/nanosandbox/sdk/go/@v/v${ver}.info" ;;
  *) echo "unknown sdk: $sdk" >&2; exit 2 ;;
esac

# Some hosts (e.g. a docs SPA) answer 200 for unknown paths, so for the JVM
# artifacts also require the body to look like a POM.
if body="$(curl -fsS -A "$UA" --max-time 30 "$url" 2>/dev/null)"; then
  if [ -z "$pattern" ] || printf '%s' "$body" | grep -qE -- "$pattern"; then
    echo "already published: $sdk $ver"
    exit 0
  fi
fi

echo "not published: $sdk $ver"
exit 1
