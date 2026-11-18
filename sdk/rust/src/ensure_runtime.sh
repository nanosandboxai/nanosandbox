#!/usr/bin/env bash
#
# ensure-runtime.sh — provision the nanosandbox runtime.
#
# Ensures the slim runtime (nanosb-runtime helper + libkrunfw + gvproxy +
# exec-agent) is present under $NANOSB_HOME, downloading + verifying it from the
# release if necessary. Idempotent: exits 0 silently when the runtime already
# exists. Invoked by each SDK's ManagedRuntime when the helper is not found.
#
# Config (env):
#   NANOSB_HOME              install root (default ~/.nanosandbox)
#   NANOSB_VERSION           runtime version to install (required to download)
#   NANOSB_RUNTIME_URL       release base URL (default the GitHub release)
#   NANOSB_RUNTIME_SHA256    expected sha256 of the bundle (optional, verified)
#   NANOSB_OFFLINE=1         never download (fail if missing)
#   NANOSB_VERIFY_COSIGN=1   verify the cosign signature when available
set -euo pipefail

home="${NANOSB_HOME:-$HOME/.nanosandbox}"
bin="$home/bin/nanosb-runtime"

# Already installed → done.
[ -x "$bin" ] && exit 0

ver="${NANOSB_VERSION:-}"
if [ -z "$ver" ]; then
  echo "ensure-runtime: runtime missing at $bin and NANOSB_VERSION is not set" >&2
  exit 1
fi
if [ "${NANOSB_OFFLINE:-0}" = "1" ]; then
  echo "ensure-runtime: runtime missing and NANOSB_OFFLINE=1" >&2
  exit 1
fi

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in Darwin) os=darwin ;; Linux) os=linux ;; *) echo "ensure-runtime: unsupported OS $os" >&2; exit 1 ;; esac
case "$arch" in arm64 | aarch64) arch=arm64 ;; x86_64 | amd64) arch=x86_64 ;; *) echo "ensure-runtime: unsupported arch $arch" >&2; exit 1 ;; esac

base="${NANOSB_RUNTIME_URL:-https://github.com/nanosandboxai/nanosandbox/releases/download}"
asset="nanosandbox-runtime-${os}-${arch}.tar.gz"
url="$base/v${ver}/${asset}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "ensure-runtime: downloading $url" >&2
curl -fsSL -o "$tmp/$asset" "$url"

if [ -n "${NANOSB_RUNTIME_SHA256:-}" ]; then
  got="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
  [ "$got" = "$NANOSB_RUNTIME_SHA256" ] || { echo "ensure-runtime: sha256 mismatch (got $got)" >&2; exit 1; }
fi

if [ "${NANOSB_VERIFY_COSIGN:-0}" = "1" ] && command -v cosign >/dev/null 2>&1; then
  if curl -fsSL -o "$tmp/$asset.sig" "$url.sig" 2>/dev/null; then
    cosign verify-blob \
      --signature "$tmp/$asset.sig" \
      --certificate-identity-regexp 'https://github.com/nanosandboxai/.*' \
      --certificate-oidc-issuer https://token.actions.githubusercontent.com \
      "$tmp/$asset" >&2
  fi
fi

mkdir -p "$home/bin" "$home/libs" "$home/guest-bin"
tar xzf "$tmp/$asset" -C "$home"
chmod +x "$home/bin/nanosb-runtime" 2>/dev/null || true
[ -f "$home/bin/gvproxy" ] && chmod +x "$home/bin/gvproxy" || true
[ -f "$home/guest-bin/exec-agent" ] && chmod +x "$home/guest-bin/exec-agent" || true

if [ "$os" = darwin ] && command -v codesign >/dev/null 2>&1; then
  codesign --force --sign - "$home/bin/nanosb-runtime" 2>/dev/null || true
fi

[ -x "$bin" ] || { echo "ensure-runtime: install failed ($bin not executable)" >&2; exit 1; }
echo "ensure-runtime: installed $ver into $home" >&2
exit 0
