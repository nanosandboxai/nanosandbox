"""Managed nanosandbox runtime: locate the helper, spawn it, and connect.

Lets a Python SDK user manage *real* sandboxes without starting the helper by
hand. The helper (`nanosb-runtime serve`) owns the microVM lifecycle; SDK calls
are protocol messages over its Unix socket.

If the helper is not installed, `ManagedRuntime` provisions it: it runs the
shared `ensure-runtime.sh` provisioner, which downloads the per-platform runtime
bundle (nanosb-runtime + libkrunfw + gvproxy + exec-agent) from the release,
verifies it, and installs it under `$NANOSB_HOME`.

Provisioning is configured by env. `NANOSB_VERSION` selects the runtime version
to download; when the user has not set it, the SDK pins it to its own release
version (read from the installed distribution metadata), so `pip install
nanosandbox-sdk==0.3.0` provisions runtime `v0.3.0`. Other knobs:
`NANOSB_HOME`, `NANOSB_RUNTIME_URL`, `NANOSB_RUNTIME_SHA256`, and
`NANOSB_OFFLINE=1` to never download.

    with ManagedRuntime() as rt:
        client = SandboxClient.connect(rt.socket)
        box = client.create({"image": "library/alpine:latest", "name": "demo"})
        print(box.exec("echo", ["hi"])["stdout"])
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Optional

# Embedded provisioner — shipped with the SDK so auto-provisioning works from an
# installed package with no repo present. Kept in sync with
# scripts/sdk/ensure-runtime.sh.
_EMBEDDED_PROVISIONER = r'''#!/usr/bin/env bash
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
  # Re-sign ad-hoc. macOS requires the com.apple.security.hypervisor entitlement
  # for Hypervisor.framework; pass it when the bundle (or repo) ships a plist so
  # the re-sign does not strip it.
  ent="$home/entitlements.plist"
  [ -f "$ent" ] || ent="$(cd "$(dirname "$0")/.." && pwd)/../entitlements.plist"
  if [ -f "$ent" ]; then
    codesign --force --sign - --entitlements "$ent" "$home/bin/nanosb-runtime" 2>/dev/null || true
  else
    codesign --force --sign - "$home/bin/nanosb-runtime" 2>/dev/null || true
  fi
fi

[ -x "$bin" ] || { echo "ensure-runtime: install failed ($bin not executable)" >&2; exit 1; }
echo "ensure-runtime: installed $ver into $home" >&2
exit 0
'''


def find_runtime_bin() -> Optional[str]:
    """Locate the nanosb-runtime helper, in order of preference."""
    env = os.environ.get("NANOSB_RUNTIME_BIN")
    if env and Path(env).exists():
        return env
    home = os.environ.get("NANOSB_HOME") or str(Path.home() / ".nanosandbox")
    candidate = Path(home) / "bin" / "nanosb-runtime"
    if candidate.exists():
        return str(candidate)
    which = shutil.which("nanosb-runtime")
    if which:
        return which
    repo = Path(__file__).resolve().parents[3]
    dev = repo / "target" / "debug" / "nanosb-runtime"
    if dev.exists():
        return str(dev)
    return None


def _sdk_version() -> str:
    """The SDK's own release version, used to pin the runtime version when the
    user has not set ``NANOSB_VERSION``. Reads the installed distribution
    metadata; falls back to the current release version when the package is not
    installed (e.g. running from a source checkout)."""
    try:
        from importlib.metadata import version as _dist_version

        return _dist_version("nanosandbox-sdk")
    except Exception:
        return "0.3.0"


def _provisioner_env() -> dict:
    """Environment for the provisioner child process.

    Inherits the caller's environment and pins ``NANOSB_VERSION`` to the SDK's
    own version when the user has not provided one, so ``pip install
    nanosandbox-sdk==0.3.0`` provisions the matching runtime ``v0.3.0``. A
    user-provided ``NANOSB_VERSION`` is never overridden."""
    env = os.environ.copy()
    if not env.get("NANOSB_VERSION"):
        env["NANOSB_VERSION"] = _sdk_version()
    return env


def _find_provisioner() -> Optional[str]:
    env = os.environ.get("NANOSB_PROVISIONER")
    if env and Path(env).exists():
        return env
    here = Path(__file__).resolve()
    for base in (here.parent, here.parents[1], here.parents[2], here.parents[3]):
        cand = base / "scripts" / "sdk" / "ensure-runtime.sh"
        if cand.exists():
            return str(cand)
    side = here.parent / "ensure-runtime.sh"
    if side.exists():
        return str(side)
    return None


def _run_provisioner() -> None:
    env = _provisioner_env()
    script = _find_provisioner()
    if script:
        subprocess.run(["bash", script], check=True, env=env)
        return
    fd, path = tempfile.mkstemp(suffix=".sh", prefix="nanosandbox-ensure-")
    with os.fdopen(fd, "w") as fh:
        fh.write(_EMBEDDED_PROVISIONER)
    try:
        subprocess.run(["bash", path], check=True, env=env)
    finally:
        os.unlink(path)


def ensure_runtime() -> Optional[str]:
    """Return the helper path, provisioning it (download+verify) if missing."""
    found = find_runtime_bin()
    if found:
        return found
    _run_provisioner()
    return find_runtime_bin()


class ManagedRuntime:
    """A running nanosb-runtime helper on a private socket."""

    def __init__(self, binary: Optional[str] = None, wait_secs: float = 15.0) -> None:
        self.binary = binary or ensure_runtime()
        if not self.binary:
            raise RuntimeError(
                "nanosb-runtime not found and could not be provisioned; set "
                "NANOSB_RUNTIME_BIN, or NANOSB_VERSION to allow auto-download"
            )
        self.dir = Path(tempfile.mkdtemp(prefix="nanosb-e2e-"))
        self.socket = self.dir / "helper.sock"
        self.proc = subprocess.Popen([self.binary, "serve", "--socket", str(self.socket)])
        deadline = time.monotonic() + wait_secs
        while not self.socket.exists():
            if self.proc.poll() is not None:
                self.close()
                raise RuntimeError("nanosb-runtime exited before opening its socket")
            if time.monotonic() > deadline:
                self.close()
                raise RuntimeError("timed out waiting for the helper socket")
            time.sleep(0.05)

    def close(self) -> None:
        proc = getattr(self, "proc", None)
        if proc is not None and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        shutil.rmtree(getattr(self, "dir", ""), ignore_errors=True)

    def __enter__(self) -> "ManagedRuntime":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()
