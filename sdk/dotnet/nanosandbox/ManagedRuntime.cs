using System;
using System.Diagnostics;
using System.IO;
using System.Threading;

/// <summary>
/// Managed nanosandbox runtime: locate the helper, spawn it, and connect.
///
/// Lets a .NET SDK user manage real sandboxes without starting the helper by
/// hand. The helper (<c>nanosb-runtime serve</c>) owns the microVM lifecycle;
/// SDK calls are protocol messages over its Unix socket.
///
/// If the helper is not installed, <c>start()</c> provisions it: it runs the
/// shared <c>ensure-runtime.sh</c> provisioner, which downloads the
/// per-platform runtime bundle (nanosb-runtime + libkrunfw + gvproxy +
/// exec-agent) from the release, verifies it, and installs it under
/// <c>$NANOSB_HOME</c>. This makes <c>dotnet add package Nanosandbox.Sdk</c>
/// the only step needed.
///
/// Provisioning is configured by env. <c>NANOSB_VERSION</c> selects the runtime
/// version to download; when the user has not set it, the SDK pins it to its
/// own version (read from the assembly), so a 0.3.0 package provisions runtime
/// v0.3.0. Other knobs: <c>NANOSB_HOME</c>, <c>NANOSB_RUNTIME_URL</c>,
/// <c>NANOSB_RUNTIME_SHA256</c>, and <c>NANOSB_OFFLINE=1</c> to never download.
/// </summary>
public sealed class ManagedRuntime : IDisposable {
    public Process Proc { get; }
    public String Dir { get; }
    public String Socket { get; }

    private ManagedRuntime(Process proc, String dir, String socket) {
        Proc = proc;
        Dir = dir;
        Socket = socket;
    }

    /// <summary>Locate the nanosb-runtime helper, in order of preference.</summary>
    public static String findRuntimeBin() {
        var env = Environment.GetEnvironmentVariable("NANOSB_RUNTIME_BIN");
        if (!String.IsNullOrEmpty(env) && File.Exists(env)) {
            return env;
        }
        var home = Environment.GetEnvironmentVariable("NANOSB_HOME")
            ?? Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".nanosandbox");
        var bundled = Path.Combine(home, "bin", "nanosb-runtime");
        if (File.Exists(bundled)) {
            return bundled;
        }
        var path = Environment.GetEnvironmentVariable("PATH");
        if (!String.IsNullOrEmpty(path)) {
            foreach (var p in path.Split(Path.PathSeparator)) {
                var c = Path.Combine(p, "nanosb-runtime");
                if (File.Exists(c)) {
                    return c;
                }
            }
        }
        var dir = Directory.GetCurrentDirectory();
        for (int i = 0; i < 8 && dir != null; i++) {
            var candidate = Path.Combine(dir, "target", "debug", "nanosb-runtime");
            if (File.Exists(candidate)) {
                return candidate;
            }
            dir = Path.GetDirectoryName(dir);
        }
        return null;
    }

    /// <summary>
    /// Embedded provisioner — shipped with the SDK so auto-provisioning works
    /// from an installed package with no repo present. Kept in sync with
    /// scripts/sdk/ensure-runtime.sh.
    /// </summary>
    private const String EmbeddedProvisioner = """
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

        """;

    /// <summary>
    /// Resolve the ensure-runtime.sh provisioner. Preference: the
    /// NANOSB_PROVISIONER env var, then scripts/sdk/ensure-runtime.sh relative
    /// to the enclosing package/repo, then an embedded copy.
    /// </summary>
    private static String findProvisioner() {
        var env = Environment.GetEnvironmentVariable("NANOSB_PROVISIONER");
        if (!String.IsNullOrEmpty(env) && File.Exists(env)) {
            return env;
        }
        var baseDir = AppContext.BaseDirectory;
        for (int i = 0; i < 8 && baseDir != null; i++) {
            var candidate = Path.Combine(baseDir, "scripts", "sdk", "ensure-runtime.sh");
            if (File.Exists(candidate)) {
                return candidate;
            }
            baseDir = Path.GetDirectoryName(baseDir);
        }
        return null;
    }

    /// <summary>
    /// Run the provisioner (repo copy if found, else the embedded copy) to
    /// download+verify+install the runtime under $NANOSB_HOME. Inherits the
    /// process environment, so NANOSB_HOME / NANOSB_VERSION / NANOSB_RUNTIME_URL
    /// / NANOSB_OFFLINE / NANOSB_RUNTIME_SHA256 flow through to the script.
    /// </summary>
    private static void runProvisioner() {
        var script = findProvisioner();
        if (script != null) {
            runBash(script);
            return;
        }
        var tmp = Path.Combine(Path.GetTempPath(), "nanosandbox-ensure-" + Guid.NewGuid().ToString("N") + ".sh");
        try {
            File.WriteAllText(tmp, EmbeddedProvisioner);
            runBash(tmp);
        } finally {
            try {
                File.Delete(tmp);
            } catch {
                // best effort
            }
        }
    }

    /// <summary>
    /// The SDK's own release version, read from the assembly. Used to pin the
    /// runtime version when the user has not set NANOSB_VERSION, so a 0.3.0
    /// package provisions the matching runtime v0.3.0.
    /// </summary>
    private static String sdkVersion() {
        var v = typeof(ManagedRuntime).Assembly.GetName().Version;
        return v == null ? "0.3.0" : v.ToString(3);
    }

    private static void runBash(String script) {
        var psi = new ProcessStartInfo("bash") { UseShellExecute = false };
        psi.ArgumentList.Add(script);
        if (!psi.Environment.TryGetValue("NANOSB_VERSION", out var v) || String.IsNullOrEmpty(v)) {
            psi.Environment["NANOSB_VERSION"] = sdkVersion();
        }
        using var proc = Process.Start(psi);
        proc.WaitForExit();
        if (proc.ExitCode != 0) {
            throw new InvalidOperationException(
                "nanosb-runtime provisioning failed (ensure-runtime.sh exited " + proc.ExitCode + ")");
        }
    }

    /// <summary>Return the helper path, provisioning it (download+verify) if missing.</summary>
    private static String ensureRuntime() {
        var found = findRuntimeBin();
        if (found != null) {
            return found;
        }
        runProvisioner();
        return findRuntimeBin();
    }

    /// <summary>Start the helper on a private socket and wait for it to listen.</summary>
    public static ManagedRuntime start() {
        var bin = ensureRuntime();
        if (bin == null) {
            throw new InvalidOperationException(
                "nanosb-runtime not found and could not be provisioned; set " +
                "NANOSB_RUNTIME_BIN, or NANOSB_VERSION to allow auto-download");
        }
        // Keep the path short: a Unix socket path must fit in sun_path (~104).
        var dir = Path.Combine("/tmp", "nanosb-e2e-" + Guid.NewGuid().ToString("N").Substring(0, 8));
        Directory.CreateDirectory(dir);
        var socket = Path.Combine(dir, "helper.sock");
        var psi = new ProcessStartInfo(bin) { UseShellExecute = false };
        psi.ArgumentList.Add("serve");
        psi.ArgumentList.Add("--socket");
        psi.ArgumentList.Add(socket);
        var proc = Process.Start(psi);
        var deadline = DateTime.UtcNow + TimeSpan.FromSeconds(15);
        while (!File.Exists(socket)) {
            if (proc.HasExited) {
                Directory.Delete(dir, true);
                throw new InvalidOperationException("nanosb-runtime exited before opening its socket");
            }
            if (DateTime.UtcNow > deadline) {
                proc.Kill(true);
                Directory.Delete(dir, true);
                throw new InvalidOperationException("timed out waiting for the helper socket");
            }
            Thread.Sleep(50);
        }
        return new ManagedRuntime(proc, dir, socket);
    }

    public void Dispose() {
        try {
            if (!Proc.HasExited) {
                Proc.Kill(true);
                Proc.WaitForExit(5000);
            }
        } catch {
            // best effort
        }
        try {
            Directory.Delete(Dir, true);
        } catch {
            // best effort
        }
    }
}
