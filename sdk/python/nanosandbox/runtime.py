"""Managed nanosandbox runtime: locate the helper, spawn it, and connect.

Lets a Python SDK user manage *real* sandboxes without starting the helper by
hand. The helper (`nanosb-runtime serve`) owns the microVM lifecycle; SDK calls
are protocol messages over its Unix socket.

If the helper is not installed, `ManagedRuntime` provisions it: it runs the
shared `ensure-runtime.sh` provisioner, which downloads the per-platform runtime
bundle (nanosb-runtime + libkrunfw + gvproxy + exec-agent) from the release,
verifies it, and installs it under `$NANOSB_HOME`.

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
set -euo pipefail
home="${NANOSB_HOME:-$HOME/.nanosandbox}"
bin="$home/bin/nanosb-runtime"
[ -x "$bin" ] && exit 0
ver="${NANOSB_VERSION:-}"
[ -n "$ver" ] || { echo "ensure-runtime: runtime missing and NANOSB_VERSION unset" >&2; exit 1; }
[ "${NANOSB_OFFLINE:-0}" = "1" ] && { echo "ensure-runtime: offline and runtime missing" >&2; exit 1; }
os="$(uname -s)"; arch="$(uname -m)"
case "$os" in Darwin) os=darwin ;; Linux) os=linux ;; *) echo "ensure-runtime: unsupported OS $os" >&2; exit 1 ;; esac
case "$arch" in arm64|aarch64) arch=arm64 ;; x86_64|amd64) arch=x86_64 ;; *) echo "ensure-runtime: unsupported arch $arch" >&2; exit 1 ;; esac
base="${NANOSB_RUNTIME_URL:-https://github.com/nanosandboxai/nanosandbox/releases/download}"
asset="nanosandbox-runtime-${os}-${arch}.tar.gz"
url="$base/v${ver}/${asset}"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
echo "ensure-runtime: downloading $url" >&2
curl -fsSL -o "$tmp/$asset" "$url"
if [ -n "${NANOSB_RUNTIME_SHA256:-}" ]; then
  got="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
  [ "$got" = "$NANOSB_RUNTIME_SHA256" ] || { echo "ensure-runtime: sha256 mismatch" >&2; exit 1; }
fi
mkdir -p "$home/bin" "$home/libs" "$home/guest-bin"
tar xzf "$tmp/$asset" -C "$home"
chmod +x "$home/bin/nanosb-runtime" 2>/dev/null || true
[ -f "$home/bin/gvproxy" ] && chmod +x "$home/bin/gvproxy" || true
[ -f "$home/guest-bin/exec-agent" ] && chmod +x "$home/guest-bin/exec-agent" || true
if [ "$os" = darwin ] && command -v codesign >/dev/null 2>&1; then
  codesign --force --sign - "$home/bin/nanosb-runtime" 2>/dev/null || true
fi
[ -x "$bin" ] || { echo "ensure-runtime: install failed" >&2; exit 1; }
echo "ensure-runtime: installed $ver into $home" >&2
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
    script = _find_provisioner()
    if script:
        subprocess.run(["bash", script], check=True)
        return
    fd, path = tempfile.mkstemp(suffix=".sh", prefix="nanosandbox-ensure-")
    with os.fdopen(fd, "w") as fh:
        fh.write(_EMBEDDED_PROVISIONER)
    try:
        subprocess.run(["bash", path], check=True)
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
