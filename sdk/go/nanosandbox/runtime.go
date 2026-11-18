package nanosandbox

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
)

// sdkVersion is the SDK's own release version, used to pin the runtime version
// when the user has not set NANOSB_VERSION, so `go get
// github.com/nanosandboxai/nanosandbox/sdk/go@v0.2.0` provisions the matching
// runtime v0.2.0.
const sdkVersion = "0.2.0"

// embeddedProvisioner is the shared ensure-runtime.sh provisioner, shipped with
// the SDK so auto-provisioning works from an installed package with no repo
// present. Kept in sync with scripts/sdk/ensure-runtime.sh.
const embeddedProvisioner = `#!/usr/bin/env bash
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
`

// FindRuntimeBin locates the nanosb-runtime helper, in order of preference:
// NANOSB_RUNTIME_BIN -> $NANOSB_HOME/bin -> PATH -> the repo target/debug build.
func FindRuntimeBin() string {
	if p := os.Getenv("NANOSB_RUNTIME_BIN"); p != "" {
		if _, err := os.Stat(p); err == nil {
			return p
		}
	}
	home := os.Getenv("NANOSB_HOME")
	if home == "" {
		if h, err := os.UserHomeDir(); err == nil {
			home = filepath.Join(h, ".nanosandbox")
		}
	}
	if cand := filepath.Join(home, "bin", "nanosb-runtime"); home != "" {
		if _, err := os.Stat(cand); err == nil {
			return cand
		}
	}
	if p, err := exec.LookPath("nanosb-runtime"); err == nil {
		return p
	}
	// Walk up from the working directory looking for the repo dev build.
	dir, err := os.Getwd()
	for i := 0; i < 8 && err == nil; i++ {
		cand := filepath.Join(dir, "target", "debug", "nanosb-runtime")
		if _, statErr := os.Stat(cand); statErr == nil {
			return cand
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			break
		}
		dir = parent
	}
	return ""
}

// ManagedRuntime is a running nanosb-runtime helper on a private socket.
type ManagedRuntime struct {
	cmd    *exec.Cmd
	Dir    string
	Socket string
}

// findProvisioner locates the ensure-runtime.sh provisioner, in order of
// preference: NANOSB_PROVISIONER -> the repo scripts/sdk/ensure-runtime.sh.
func findProvisioner() string {
	if p := os.Getenv("NANOSB_PROVISIONER"); p != "" {
		if _, err := os.Stat(p); err == nil {
			return p
		}
	}
	dir, err := os.Getwd()
	for i := 0; i < 8 && err == nil; i++ {
		cand := filepath.Join(dir, "scripts", "sdk", "ensure-runtime.sh")
		if _, statErr := os.Stat(cand); statErr == nil {
			return cand
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			break
		}
		dir = parent
	}
	return ""
}

// provisionerEnv returns the environment for the provisioner child process: the
// caller's environment with NANOSB_VERSION pinned to the SDK's own version when
// the user has not provided one. A user-provided NANOSB_VERSION is never
// overridden.
func provisionerEnv() []string {
	env := os.Environ()
	for _, kv := range env {
		if strings.HasPrefix(kv, "NANOSB_VERSION=") && len(kv) > len("NANOSB_VERSION=") {
			return env
		}
	}
	return append(env, "NANOSB_VERSION="+sdkVersion)
}

// runProvisioner runs the embedded provisioner (or a located script) to
// download+verify+install the runtime under $NANOSB_HOME.
func runProvisioner() error {
	script := findProvisioner()
	if script == "" {
		f, err := os.CreateTemp("", "nanosandbox-ensure-*.sh")
		if err != nil {
			return err
		}
		path := f.Name()
		if _, werr := f.WriteString(embeddedProvisioner); werr != nil {
			f.Close()
			os.Remove(path)
			return werr
		}
		if cerr := f.Close(); cerr != nil {
			os.Remove(path)
			return cerr
		}
		defer os.Remove(path)
		script = path
	}
	cmd := exec.Command("bash", script)
	cmd.Env = provisionerEnv()
	cmd.Stdout = os.Stderr
	cmd.Stderr = os.Stderr
	return cmd.Run()
}

// ensureRuntime returns the helper path, provisioning it (download+verify) if
// missing.
func ensureRuntime() (string, error) {
	if bin := FindRuntimeBin(); bin != "" {
		return bin, nil
	}
	if err := runProvisioner(); err != nil {
		return "", err
	}
	if bin := FindRuntimeBin(); bin != "" {
		return bin, nil
	}
	return "", fmt.Errorf("nanosb-runtime not found after provisioning")
}

// StartRuntime locates the helper (provisioning it if missing), serves it on a
// temp socket, and waits for the socket to appear.
func StartRuntime(wait time.Duration) (*ManagedRuntime, error) {
	bin, err := ensureRuntime()
	if err != nil {
		return nil, fmt.Errorf(
			"nanosb-runtime not found and could not be provisioned; set "+
				"NANOSB_RUNTIME_BIN, or NANOSB_VERSION to allow auto-download: %w",
			err,
		)
	}
	dir, err := os.MkdirTemp("", "nanosb-e2e-")
	if err != nil {
		return nil, err
	}
	socket := filepath.Join(dir, "helper.sock")
	cmd := exec.Command(bin, "serve", "--socket", socket)
	if err := cmd.Start(); err != nil {
		os.RemoveAll(dir)
		return nil, err
	}
	deadline := time.Now().Add(wait)
	for {
		if _, err := os.Stat(socket); err == nil {
			return &ManagedRuntime{cmd: cmd, Dir: dir, Socket: socket}, nil
		}
		if cmd.ProcessState != nil && cmd.ProcessState.Exited() {
			os.RemoveAll(dir)
			return nil, fmt.Errorf("nanosb-runtime exited before opening its socket")
		}
		if time.Now().After(deadline) {
			_ = cmd.Process.Kill()
			os.RemoveAll(dir)
			return nil, fmt.Errorf("timed out waiting for the helper socket")
		}
		time.Sleep(50 * time.Millisecond)
	}
}

// Close kills the helper and removes its temp directory.
func (r *ManagedRuntime) Close() {
	if r.cmd != nil && r.cmd.Process != nil {
		_ = r.cmd.Process.Kill()
		_ = r.cmd.Wait()
	}
	if r.Dir != "" {
		_ = os.RemoveAll(r.Dir)
	}
}
