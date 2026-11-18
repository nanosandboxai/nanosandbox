package nanosandbox

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"time"
)

// embeddedProvisioner is the shared ensure-runtime.sh provisioner, shipped with
// the SDK so auto-provisioning works from an installed package with no repo
// present. Kept in sync with scripts/sdk/ensure-runtime.sh.
const embeddedProvisioner = `#!/usr/bin/env bash
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
