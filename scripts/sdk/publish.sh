#!/usr/bin/env bash
#
# publish.sh <sdk> <version> [--dry-run]
#
# Idempotent per-ecosystem publish driver. Skips an SDK whose version is
# already published (check-published.sh). In --dry-run mode it only builds the
# package/artifact and never uploads.
#
# The JVM SDKs build a Maven repository layout under dist/maven-repo (the
# release workflow publishes that directory to GitHub Pages); Go creates the
# sdk/go/v<version> module tag.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
sdk="${1:?usage: publish.sh <sdk> <version> [--dry-run]}"
ver="${2:?usage: publish.sh <sdk> <version> [--dry-run]}"
dry=0
[ "${3:-}" = "--dry-run" ] && dry=1

# --dry-run always builds for inspection (AC6); only a real publish skips.
if [ "$dry" = 0 ] && "$here/check-published.sh" "$sdk" "$ver" >/dev/null 2>&1; then
  echo "skip: $sdk $ver already published"
  exit 0
fi

note() { printf 'publish[%s] %s\n' "$sdk" "$*"; }

# Publish one crate, retrying ONLY transient failures (network / 429 / 5xx).
# Permanent errors (auth, unverified email, duplicate version, invalid manifest)
# fail immediately — retrying them just burns time and hides the real cause.
publish_crate() {
  local c="$1" attempt out
  for attempt in 1 2 3 4 5; do
    if out="$(cargo publish -p "$c" 2>&1)"; then
      printf '%s\n' "$out"
      return 0
    fi
    printf '%s\n' "$out" >&2
    if printf '%s' "$out" | grep -qiE 'rate.?limit|too many requests|status (408|429|5[0-9][0-9])|timed out|connection (reset|refused|closed)|could not connect|network|temporary failure|error sending request'; then
      note "transient error publishing $c; retrying (attempt $attempt)"
      sleep $((attempt * 15))
    else
      note "permanent error publishing $c; not retrying"
      return 1
    fi
  done
  return 1
}

case "$sdk" in
  rust)
    if [ "$dry" = 1 ]; then
      note "cargo package -p nanosb-protocol"
      cargo package -p nanosb-protocol --allow-dirty
      note "dry-run: nanosb-client + nanosandbox-sdk require nanosb-protocol on crates.io (skipped)"
    else
      for c in nanosb-protocol nanosb-client nanosandbox-sdk; do
        note "cargo publish -p $c"
        publish_crate "$c"
        sleep 20
      done
    fi
    ;;

  python)
    mkdir -p "$root/dist/python"
    note "build sdist+wheel"
    if python3 -m build --version >/dev/null 2>&1; then
      python3 -m build "$root/sdk/python" --outdir "$root/dist/python"
    else
      python3 -m pip wheel "$root/sdk/python" -w "$root/dist/python" --no-deps
    fi
    if [ "$dry" = 0 ]; then
      note "twine upload"
      python3 -m twine upload --non-interactive "$root"/dist/python/*
    fi
    ;;

  typescript|npm)
    mkdir -p "$root/dist/npm"
    note "npm pack"
    (cd "$root/sdk/typescript" && npm pack --pack-destination "$root/dist/npm")
    if [ "$dry" = 0 ]; then
      note "npm publish"
      (cd "$root/sdk/typescript" && npm publish)
    fi
    ;;

  ruby)
    mkdir -p "$root/dist/ruby"
    note "gem build"
    (cd "$root/sdk/ruby" && gem build nanosandbox.gemspec --output "$root/dist/ruby/nanosandbox-sdk-$ver.gem")
    if [ "$dry" = 0 ]; then
      note "gem push"
      GEM_HOST_API_KEY="$(printf '%s' "${GEM_HOST_API_KEY:-}" | tr -d '\r\n')" \
        gem push "$root/dist/ruby/nanosandbox-sdk-$ver.gem"
    fi
    ;;

  dotnet)
    note "dotnet pack"
    dotnet pack "$root/sdk/dotnet/nanosandbox/Nanosandbox.csproj" -c Release -o "$root/dist/dotnet"
    if [ "$dry" = 0 ]; then
      note "dotnet nuget push"
      dotnet nuget push "$root/dist/dotnet/Nanosandbox.Sdk.$ver.nupkg" \
        --api-key "${NUGET_API_KEY:?NUGET_API_KEY is required}" \
        --source https://api.nuget.org/v3/index.json
    fi
    ;;

  java)
    mkdir -p "$root/dist/maven-repo"
    note "mvn package + build Maven layout"
    mvn -q -B -f "$root/sdk/java/pom.xml" -DskipTests package
    mvn -q -B -f "$root/sdk/java/pom.xml" \
      -DaltDeploymentRepository="pages::default::file://$root/dist/maven-repo" \
      deploy
    ;;

  kotlin)
    mkdir -p "$root/dist/maven-repo"
    note "gradle jar + publish layout"
    (cd "$root/sdk/kotlin" && ./gradlew --no-daemon jar)
    (cd "$root/sdk/kotlin" && ./gradlew --no-daemon publish \
      -PmavenRepoUrl="file://$root/dist/maven-repo")
    ;;

  go)
    if [ "$dry" = 1 ]; then
      note "dry-run: would tag sdk/go/v$ver"
      go list -m "github.com/nanosandboxai/nanosandbox/sdk/go" >/dev/null 2>&1 || true
    else
      note "tag sdk/go/v$ver"
      git tag "sdk/go/v$ver"
      git push origin "sdk/go/v$ver"
      note "warm proxy.golang.org"
      curl -fsS "https://proxy.golang.org/github.com/nanosandboxai/nanosandbox/sdk/go/@v/v$ver.info" >/dev/null || true
    fi
    ;;

  *)
    echo "unknown sdk: $sdk" >&2
    exit 2
    ;;
esac

note "done"
