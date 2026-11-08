#!/usr/bin/env bash
#
# package-sdk.sh — bundle the macOS arm64 runtime with each SDK (WS9 / AC9).
#
# Produces, per SDK, a self-contained package under dist/ containing:
#   bin/nanosb-runtime     the slim helper (pre-signed)
#   bin/gvproxy            networking sidecar
#   libs/libkrunfw.5.dylib kernel firmware
#   guest-bin/exec-agent   in-guest exec agent
#   sdk/<lang>/            the SDK sources
#   install.sh             lays the runtime into $NANOSB_HOME (lazy re-sign)
#   LICENSE                the repository license (Apache-2.0)
#   README.md              what this package is
#
# Usage:
#   scripts/package-sdk.sh                 # all SDKs
#   scripts/package-sdk.sh rust python     # a subset
#   NANOSB_HOME=~/.nanosandbox scripts/package-sdk.sh
#
# The helper is code-signed at build time; install.sh re-signs lazily if the
# signature is invalid (e.g. after being copied through a quarantine flag).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="${DIST:-$REPO_ROOT/dist}"
PROFILE="${PROFILE:-release}"
NANOSB_HOME="${NANOSB_HOME:-$HOME/.nanosandbox}"
# Source of the runtime deps to bundle (the developer's local install).
SRC_HOME="${NANOSB_SRC_HOME:-$NANOSB_HOME}"

SDKS=("$@")
if [ ${#SDKS[@]} -eq 0 ]; then
  SDKS=(rust python typescript go ruby java kotlin dotnet)
fi

PROFILE_ARG=""
TARGET_DIR="$REPO_ROOT/target/debug"
if [ -n "$PROFILE" ] && [ "$PROFILE" != "dev" ] && [ "$PROFILE" != "debug" ]; then
  PROFILE_ARG="--profile $PROFILE"
  TARGET_DIR="$REPO_ROOT/target/$PROFILE"
fi

echo "==> building the slim helper (${PROFILE:-dev})"
cargo build -p nanosb-cli --bin nanosb-runtime --no-default-features --features runtime-host $PROFILE_ARG
HELPER="$TARGET_DIR/nanosb-runtime"
[ -x "$HELPER" ] || { echo "error: helper not built at $HELPER"; exit 1; }

LIBKRUNFW="$(ls "$SRC_HOME"/libs/libkrunfw*.dylib 2>/dev/null | head -1 || true)"
GVPROXY="$SRC_HOME/bin/gvproxy"
GUEST_BIN="${NANOSB_GUEST_BIN_DIR:-$SRC_HOME/guest-bin}"
EXEC_AGENT="$GUEST_BIN/exec-agent"
[ -n "$LIBKRUNFW" ] || echo "warning: no libkrunfw*.dylib under $SRC_HOME/libs (bundle will omit it)"
[ -f "$GVPROXY" ]  || echo "warning: no gvproxy under $SRC_HOME/bin (bundle will omit it)"
[ -f "$EXEC_AGENT" ] || echo "warning: no exec-agent under $GUEST_BIN (bundle will omit it)"

echo "==> pre-signing the helper"
if command -v codesign >/dev/null 2>&1; then
  codesign --force --sign - --entitlements "$REPO_ROOT/entitlements.plist" "$HELPER" \
    || codesign --force --sign - "$HELPER" \
    || echo "warning: codesign failed; install.sh will re-sign lazily"
fi

mkdir -p "$DIST"

for sdk in "${SDKS[@]}"; do
  out="$DIST/nanosandbox-sdk-$sdk"
  rm -rf "$out"
  mkdir -p "$out/bin" "$out/libs" "$out/sdk" "$out/guest-bin"

  # Every distributable bundle carries the repository LICENSE.
  if [ -f "$REPO_ROOT/LICENSE" ]; then
    cp "$REPO_ROOT/LICENSE" "$out/LICENSE"
  else
    echo "warning: $REPO_ROOT/LICENSE not found; bundle omits the license" >&2
  fi

  cp "$HELPER" "$out/bin/nanosb-runtime"; chmod +x "$out/bin/nanosb-runtime"
  if [ -n "$LIBKRUNFW" ]; then cp "$LIBKRUNFW" "$out/libs/"; fi
  if [ -f "$GVPROXY" ]; then cp "$GVPROXY" "$out/bin/gvproxy"; chmod +x "$out/bin/gvproxy"; fi
  if [ -f "$EXEC_AGENT" ]; then cp "$EXEC_AGENT" "$out/guest-bin/exec-agent"; chmod +x "$out/guest-bin/exec-agent"; fi

  if [ -d "$REPO_ROOT/sdk/$sdk" ]; then
    rsync -a \
      --exclude target --exclude classes --exclude '*.jar' --exclude '*.class' \
      --exclude bin --exclude obj --exclude __pycache__ --exclude '*.pyc' \
      --exclude build --exclude .gradle --exclude node_modules --exclude dist \
      --exclude '*.egg-info' --exclude '*.nupkg' --exclude '*.snupkg' \
      --exclude '*.gem' --exclude '*.whl' \
      "$REPO_ROOT/sdk/$sdk/" "$out/sdk/"
  fi

  cp "$REPO_ROOT/scripts/package-install.sh" "$out/install.sh"; chmod +x "$out/install.sh"
  cat > "$out/README.md" <<EOF
# nanosandbox SDK package — $sdk (macOS arm64)

Self-contained bundle: the \`nanosb-runtime\` helper, \`gvproxy\`, and
\`libkrunfw.5.dylib\`, plus the \`$sdk\` SDK sources.

## Install the bundled runtime

\`\`\`bash
./install.sh            # lays the runtime into \$NANOSB_HOME (default ~/.nanosandbox)
\`\`\`

Then point the SDK at the helper socket (or run the helper directly).
See \`docs/runtime-setup.md\` in the repository.
EOF

  echo "packaged $out"
done

echo "==> done: $DIST"