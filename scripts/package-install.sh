#!/usr/bin/env bash
#
# package-install.sh — install the bundled runtime into $NANOSB_HOME.
#
# Copied into each dist bundle by scripts/package-sdk.sh. Lays out:
#   $NANOSB_HOME/bin/nanosb-runtime
#   $NANOSB_HOME/bin/gvproxy
#   $NANOSB_HOME/libs/libkrunfw.5.dylib
#   $NANOSB_HOME/guest-bin/exec-agent
#   $NANOSB_HOME/LICENSE
# and re-signs the helper lazily if its signature is missing/invalid.
# Idempotent: safe to re-run over an existing $NANOSB_HOME.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
NANOSB_HOME="${NANOSB_HOME:-$HOME/.nanosandbox}"

mkdir -p "$NANOSB_HOME/bin" "$NANOSB_HOME/libs" "$NANOSB_HOME/guest-bin"

cp -f "$HERE/bin/nanosb-runtime" "$NANOSB_HOME/bin/nanosb-runtime"; chmod +x "$NANOSB_HOME/bin/nanosb-runtime"
if [ -f "$HERE/bin/gvproxy" ]; then
  cp -f "$HERE/bin/gvproxy" "$NANOSB_HOME/bin/gvproxy"; chmod +x "$NANOSB_HOME/bin/gvproxy"
fi
if ls "$HERE"/libs/libkrunfw*.dylib >/dev/null 2>&1; then
  cp -f "$HERE"/libs/libkrunfw*.dylib "$NANOSB_HOME/libs/"
fi
if [ -f "$HERE/guest-bin/exec-agent" ]; then
  cp -f "$HERE/guest-bin/exec-agent" "$NANOSB_HOME/guest-bin/exec-agent"; chmod +x "$NANOSB_HOME/guest-bin/exec-agent"
fi
if [ -f "$HERE/LICENSE" ]; then
  cp -f "$HERE/LICENSE" "$NANOSB_HOME/LICENSE"
fi

# Lazy re-sign: only re-sign (ad-hoc, WITH the hypervisor entitlement) when the
# helper is not already validly signed with that entitlement. Re-signing an
# already-Developer-ID-signed / notarized helper ad-hoc would strip its signature
# and invalidate notarization, so a valid signature is left alone. An ad-hoc
# re-sign without --entitlements would leave the entitlement missing, so the
# entitlement plist is always applied when re-signing.
if command -v codesign >/dev/null 2>&1; then
  helper="$NANOSB_HOME/bin/nanosb-runtime"
  if ! { codesign --verify --deep --strict "$helper" 2>/dev/null \
           && codesign -d --entitlements :- "$helper" 2>/dev/null \
                | grep -q 'com.apple.security.hypervisor'; }; then
    echo "re-signing helper (lazy)"
    xattr -dr com.apple.quarantine "$helper" 2>/dev/null || true
    entitlements="$(mktemp /tmp/nanosb-entitlements.XXXXXX.plist)"
    cat > "$entitlements" << 'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.hypervisor</key>
    <true/>
</dict>
</plist>
PLIST
    codesign --force --sign - --entitlements "$entitlements" "$helper" \
      || echo "warning: re-sign failed; the helper may need manual signing"
    rm -f "$entitlements"
  fi
fi

echo "runtime installed into $NANOSB_HOME"
echo "  bin/nanosb-runtime, bin/gvproxy, libs/libkrunfw.5.dylib, guest-bin/exec-agent"