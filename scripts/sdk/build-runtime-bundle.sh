#!/usr/bin/env bash
#
# build-runtime-bundle.sh — assemble the SDK runtime bundle for the CURRENT platform.
#
# Produces `nanosandbox-runtime-<os>-<arch>.tar.gz` (+ `.sha256` sidecar), the
# artifact the SDK provisioner (`scripts/sdk/ensure-runtime.sh`) downloads from
# `$NANOSB_RUNTIME_URL/v$NANOSB_VERSION/` and unpacks into `$NANOSB_HOME`.
#
# Bundle layout (tar roots — unpack straight into $NANOSB_HOME):
#   bin/nanosb-runtime     the slim protocol helper
#   bin/gvproxy            user-mode networking sidecar
#   libs/libkrunfw*.dylib  kernel firmware (macOS) / libkrunfw*.so* (Linux)
#   guest-bin/exec-agent   in-guest exec agent
#   entitlements.plist     macOS hypervisor entitlement (bundled on all platforms)
#
# The SDK provisioner and `scripts/install.sh` install this SAME bundle into the
# SAME `$NANOSB_HOME`, so running either satisfies the other.
#
# Sources (all must already be built / present):
#   helper      $NANOSB_RUNTIME_BIN or <repo>/target/release/nanosb-runtime
#   gvproxy     $NANOSB_SRC_HOME/bin/gvproxy          (default: $NANOSB_HOME)
#   libkrunfw   $NANOSB_SRC_HOME/libs/libkrunfw*      (default: $NANOSB_HOME)
#   exec-agent  $NANOSB_SRC_HOME/guest-bin/exec-agent (default: $NANOSB_HOME)
#   plist       <repo>/entitlements.plist
#
# On macOS the helper is re-signed ad-hoc with the hypervisor entitlement before
# packaging; ensure-runtime.sh re-signs lazily on the target machine too.
#
# Usage:
#   scripts/sdk/build-runtime-bundle.sh <version> [--out DIR]
#
#   <version>  the release version this bundle targets (informational)
#   --out DIR  output directory (default: <repo>/dist)
#
# Prints the artifact path and its sha256 on the last two lines.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"

version="${1:-}"
[ -n "$version" ] || { echo "usage: $0 <version> [--out DIR]" >&2; exit 1; }
shift

out="${OUT_DIR:-$repo_root/dist}"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --out) out="${2:?--out requires a DIR}"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

# ─── Platform (matches ensure-runtime.sh normalization) ──────────────────────
raw_os="$(uname -s)"
raw_arch="$(uname -m)"
case "$raw_os" in
  Darwin) os=darwin ;;
  Linux)  os=linux ;;
  *) echo "build-runtime-bundle: unsupported OS $raw_os" >&2; exit 1 ;;
esac
case "$raw_arch" in
  arm64 | aarch64) arch=arm64 ;;
  x86_64 | amd64)  arch=x86_64 ;;
  *) echo "build-runtime-bundle: unsupported arch $raw_arch" >&2; exit 1 ;;
esac

# ─── Sources ─────────────────────────────────────────────────────────────────
src_home="${NANOSB_SRC_HOME:-${NANOSB_HOME:-$HOME/.nanosandbox}}"
helper="${NANOSB_RUNTIME_BIN:-$repo_root/target/release/nanosb-runtime}"
gvproxy="$src_home/bin/gvproxy"
exec_agent="$src_home/guest-bin/exec-agent"
entitlements="$repo_root/entitlements.plist"

if [ "$os" = darwin ]; then
  libkrunfw_candidates=( "$src_home"/libs/libkrunfw*.dylib )
else
  libkrunfw_candidates=( "$src_home"/libs/libkrunfw*.so* )
fi
libkrunfw=()
for f in "${libkrunfw_candidates[@]}"; do
  [ -e "$f" ] || continue
  case "$(basename "$f")" in
    *.bak*) continue ;;
  esac
  libkrunfw+=("$f")
done

[ -x "$helper" ] || { echo "build-runtime-bundle: missing helper $helper" >&2; echo "  build it with: cargo build -p nanosb-cli --bin nanosb-runtime --no-default-features --features runtime-host --release" >&2; exit 1; }
[ -x "$gvproxy" ] || { echo "build-runtime-bundle: missing gvproxy $gvproxy" >&2; exit 1; }
[ "${#libkrunfw[@]}" -gt 0 ] || { echo "build-runtime-bundle: no libkrunfw under $src_home/libs/" >&2; exit 1; }
[ -x "$exec_agent" ] || { echo "build-runtime-bundle: missing exec-agent $exec_agent" >&2; exit 1; }
[ -f "$entitlements" ] || { echo "build-runtime-bundle: missing $entitlements" >&2; exit 1; }

echo "==> building nanosandbox-runtime-$os-$arch (v${version}) from:"
echo "    helper      $helper"
echo "    gvproxy     $gvproxy"
echo "    libkrunfw   ${libkrunfw[*]}"
echo "    exec-agent  $exec_agent"

# Pre-sign the helper with the hypervisor entitlement (ad-hoc; lazy re-sign
# happens in ensure-runtime.sh / install.sh on the target too).
if [ "$os" = darwin ] && command -v codesign >/dev/null 2>&1; then
  codesign --force --sign - --entitlements "$entitlements" "$helper" 2>/dev/null \
    || codesign --force --sign - "$helper" 2>/dev/null \
    || echo "warning: codesign failed on the helper (will be re-signed on install)" >&2
fi

# ─── Stage + package ─────────────────────────────────────────────────────────
asset="nanosandbox-runtime-${os}-${arch}.tar.gz"
mkdir -p "$out"
staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT

mkdir -p "$staging/bin" "$staging/libs" "$staging/guest-bin"
cp "$helper" "$staging/bin/nanosb-runtime"
cp "$gvproxy" "$staging/bin/gvproxy"
cp "${libkrunfw[@]}" "$staging/libs/"
cp "$exec_agent" "$staging/guest-bin/exec-agent"
cp "$entitlements" "$staging/entitlements.plist"
chmod +x "$staging/bin/nanosb-runtime" "$staging/bin/gvproxy" "$staging/guest-bin/exec-agent"

tar czf "$out/$asset" -C "$staging" bin libs guest-bin entitlements.plist

if command -v sha256sum >/dev/null 2>&1; then
  sha="$(sha256sum "$out/$asset" | awk '{print $1}')"
else
  sha="$(shasum -a 256 "$out/$asset" | awk '{print $1}')"
fi
printf '%s  %s\n' "$sha" "$asset" > "$out/$asset.sha256"

echo "==> packaged $out/$asset"
echo "artifact: $out/$asset"
echo "sha256:   $sha"
