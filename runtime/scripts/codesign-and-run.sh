#!/bin/bash
# Codesign the binary with Hypervisor.framework entitlement before running.
# This is needed because libkrun uses Apple's Hypervisor.framework which
# requires the com.apple.security.hypervisor entitlement on macOS.
#
# Used as a Cargo "runner" so that `cargo run` automatically codesigns
# after each compilation, before execution.

BINARY="$1"
shift

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ENTITLEMENTS="$SCRIPT_DIR/../entitlements.plist"

if [ -f "$ENTITLEMENTS" ]; then
    codesign --force --sign - --entitlements "$ENTITLEMENTS" "$BINARY" 2>/dev/null
fi

exec "$BINARY" "$@"
