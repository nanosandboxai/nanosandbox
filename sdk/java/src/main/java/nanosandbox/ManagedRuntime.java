package nanosandbox;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Comparator;
import java.util.Map;
import java.util.concurrent.TimeUnit;
import java.util.stream.Stream;

/**
 * Managed nanosandbox runtime: locate the helper, spawn it, and connect.
 *
 * <p>Lets a Java SDK user manage real sandboxes without starting the helper by
 * hand. The helper ({@code nanosb-runtime serve}) owns the microVM lifecycle;
 * SDK calls are protocol messages over its Unix socket.
 *
 * <p>If the helper is not installed, {@link #start()} provisions it: it runs the
 * shared {@code ensure-runtime.sh} provisioner, which downloads the per-platform
 * runtime bundle (nanosb-runtime + libkrunfw + gvproxy + exec-agent) from the
 * release, verifies it, and installs it under {@code $NANOSB_HOME}. The
 * provisioner is resolved from {@code NANOSB_PROVISIONER}, then the repository's
 * {@code scripts/sdk/ensure-runtime.sh}, then an embedded copy shipped with the
 * SDK.
 *
 * <p>Provisioning is configured by env: {@code NANOSB_VERSION} selects the
 * runtime version to download; when the user has not set it, the SDK pins it to
 * its own version ({@link #SDK_VERSION}), so a {@code 0.3.0} artifact provisions
 * runtime v0.3.0. Other knobs: {@code NANOSB_HOME}, {@code NANOSB_RUNTIME_URL},
 * {@code NANOSB_RUNTIME_SHA256}, and {@code NANOSB_OFFLINE=1} to never download.
 *
 * <pre>{@code
 * try (ManagedRuntime rt = ManagedRuntime.start()) {
 *   SandboxClient client = SandboxClient.connect(rt.socket.toString());
 *   Sandbox box = client.createWith(new SandboxBuilder("library/alpine:latest").withName("demo"));
 *   System.out.println(box.exec("echo", new String[]{"hi"}).getStr("stdout"));
 * }
 * }</pre>
 */
public final class ManagedRuntime implements AutoCloseable {
  /**
   * The SDK's own release version, used to pin the runtime version when the user
   * has not set {@code NANOSB_VERSION}, so {@code ai.nanosandbox:nanosandbox-sdk:0.3.0}
   * provisions the matching runtime v0.3.0. The Kotlin SDK reuses this class.
   */
  static final String SDK_VERSION = "0.3.0";

  private final Process proc;
  public final Path dir;
  public final Path socket;

  private ManagedRuntime(Process proc, Path dir, Path socket) {
    this.proc = proc;
    this.dir = dir;
    this.socket = socket;
  }

  /** Locate the nanosb-runtime helper, in order of preference. */
  public static String findRuntimeBin() {
    String env = System.getenv("NANOSB_RUNTIME_BIN");
    if (env != null && Files.exists(Path.of(env))) {
      return env;
    }
    String home = System.getenv("NANOSB_HOME");
    if (home == null) {
      home = System.getProperty("user.home") + "/.nanosandbox";
    }
    Path bundled = Path.of(home, "bin", "nanosb-runtime");
    if (Files.exists(bundled)) {
      return bundled.toString();
    }
    String path = System.getenv("PATH");
    if (path != null) {
      for (String p : path.split(":")) {
        Path c = Path.of(p, "nanosb-runtime");
        if (Files.isExecutable(c)) {
          return c.toString();
        }
      }
    }
    Path d = Path.of("").toAbsolutePath();
    for (int i = 0; i < 8 && d != null; i++) {
      Path c = d.resolve("target/debug/nanosb-runtime");
      if (Files.exists(c)) {
        return c.toString();
      }
      d = d.getParent();
    }
    return null;
  }

  /**
   * Embedded provisioner — shipped with the SDK so auto-provisioning works from
   * an installed package with no repo present. Kept in sync with
   * scripts/sdk/ensure-runtime.sh.
   */
  private static final String EMBEDDED_PROVISIONER =
      """
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
          cosign verify-blob \\
            --signature "$tmp/$asset.sig" \\
            --certificate-identity-regexp 'https://github.com/nanosandboxai/.*' \\
            --certificate-oidc-issuer https://token.actions.githubusercontent.com \\
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

  /**
   * Resolve the {@code ensure-runtime.sh} provisioner. Preference: the
   * {@code NANOSB_PROVISIONER} env var, then {@code scripts/sdk/ensure-runtime.sh}
   * relative to the enclosing package/repo, then an embedded copy. Returns
   * {@code null} only when the embedded copy is the only option (handled by the
   * caller). Self-contained so the Kotlin SDK can reuse this class unchanged.
   */
  static String findProvisioner() {
    String env = System.getenv("NANOSB_PROVISIONER");
    if (env != null && Files.exists(Path.of(env))) {
      return env;
    }
    Path here = Path.of("").toAbsolutePath();
    for (int i = 0; i < 8 && here != null; i++) {
      Path cand = here.resolve("scripts/sdk/ensure-runtime.sh");
      if (Files.exists(cand)) {
        return cand.toString();
      }
      here = here.getParent();
    }
    return null;
  }

  /** Run the provisioner (repo copy if found, else the embedded copy). */
  private static void runProvisioner() throws IOException {
    String script = findProvisioner();
    if (script == null) {
      Path tmp = Files.createTempFile("nanosandbox-ensure-", ".sh");
      try {
        Files.writeString(tmp, EMBEDDED_PROVISIONER);
        runBash(tmp.toString());
      } finally {
        Files.deleteIfExists(tmp);
      }
    } else {
      runBash(script);
    }
  }

  private static void runBash(String script) throws IOException {
    ProcessBuilder pb = new ProcessBuilder("bash", script);
    Map<String, String> penv = pb.environment();
    if (penv.getOrDefault("NANOSB_VERSION", "").isEmpty()) {
      penv.put("NANOSB_VERSION", SDK_VERSION);
    }
    pb.inheritIO();
    Process proc = pb.start();
    try {
      if (!proc.waitFor(120, TimeUnit.SECONDS) || proc.exitValue() != 0) {
        if (proc.isAlive()) {
          proc.destroyForcibly();
        }
        throw new IOException("ensure-runtime provisioning failed (exit "
            + proc.exitValue() + ")");
      }
    } catch (InterruptedException e) {
      Thread.currentThread().interrupt();
      throw new IOException("interrupted while provisioning runtime", e);
    }
  }

  /** Return the helper path, provisioning it (download+verify) if missing. */
  static String ensureRuntime() throws IOException {
    String found = findRuntimeBin();
    if (found != null) {
      return found;
    }
    runProvisioner();
    return findRuntimeBin();
  }

  /** Start the helper on a private socket and wait for it to listen. */
  public static ManagedRuntime start() throws IOException {
    String bin = ensureRuntime();
    if (bin == null) {
      throw new IOException(
          "nanosb-runtime not found and could not be provisioned; set "
              + "NANOSB_RUNTIME_BIN, or NANOSB_VERSION to allow auto-download");
    }
    Path dir = Files.createTempDirectory("nanosb-e2e-");
    Path socket = dir.resolve("helper.sock");
    Process proc =
        new ProcessBuilder(bin, "serve", "--socket", socket.toString())
            .redirectOutput(ProcessBuilder.Redirect.DISCARD)
            .redirectError(ProcessBuilder.Redirect.DISCARD)
            .start();
    long deadline = System.currentTimeMillis() + 15_000;
    while (!Files.exists(socket)) {
      if (!proc.isAlive()) {
        deleteTree(dir);
        throw new IOException("nanosb-runtime exited before opening its socket");
      }
      if (System.currentTimeMillis() > deadline) {
        proc.destroyForcibly();
        deleteTree(dir);
        throw new IOException("timed out waiting for the helper socket");
      }
      try {
        Thread.sleep(50);
      } catch (InterruptedException e) {
        Thread.currentThread().interrupt();
      }
    }
    return new ManagedRuntime(proc, dir, socket);
  }

  @Override
  public void close() {
    proc.destroy();
    try {
      if (!proc.waitFor(5, TimeUnit.SECONDS)) {
        proc.destroyForcibly();
      }
    } catch (InterruptedException e) {
      Thread.currentThread().interrupt();
    }
    deleteTree(dir);
  }

  private static void deleteTree(Path dir) {
    if (dir == null || !Files.exists(dir)) {
      return;
    }
    try (Stream<Path> s = Files.walk(dir)) {
      s.sorted(Comparator.reverseOrder())
          .forEach(
              p -> {
                try {
                  Files.deleteIfExists(p);
                } catch (IOException ignored) {
                  // best effort
                }
              });
    } catch (IOException ignored) {
      // best effort
    }
  }
}