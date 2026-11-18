# frozen_string_literal: true

# Managed nanosandbox runtime: locate the helper, spawn it, and connect.
#
# Lets a Ruby SDK user manage real sandboxes without starting the helper by
# hand. The helper (`nanosb-runtime serve`) owns the microVM lifecycle; SDK
# calls are protocol messages over its Unix socket.
#
# If the helper is not installed, `ManagedRuntime` provisions it: it runs the
# shared `ensure-runtime.sh` provisioner, which downloads the per-platform
# runtime bundle (nanosb-runtime + libkrunfw + gvproxy + exec-agent) from the
# release, verifies it, and installs it under `$NANOSB_HOME`.
#
#   Nanosandbox::ManagedRuntime.open do |rt|
#     client = Nanosandbox::SandboxClient.connect(rt.socket)
#     box = client.create("image" => "library/alpine:latest", "name" => "demo")
#     puts box.exec("echo", ["hi"])["stdout"]
#   end

require "tmpdir"
require "tempfile"
require "fileutils"

module Nanosandbox
  # Embedded provisioner — shipped with the SDK so auto-provisioning works from
  # an installed gem with no repo present. Kept in sync with
  # scripts/sdk/ensure-runtime.sh.
  EMBEDDED_PROVISIONER = <<~'BASH'
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
  BASH

  # Locate the nanosb-runtime helper, in order of preference.
  def self.find_runtime_bin
    env = ENV["NANOSB_RUNTIME_BIN"]
    return env if env && File.exist?(env)

    home = ENV["NANOSB_HOME"] || File.join(Dir.home, ".nanosandbox")
    candidate = File.join(home, "bin", "nanosb-runtime")
    return candidate if File.exist?(candidate)

    which = `which nanosb-runtime 2>/dev/null`.strip
    return which unless which.empty?

    repo = File.expand_path("../../../..", __dir__) # lib/nanosandbox/runtime.rb -> repo root
    dev = File.join(repo, "target", "debug", "nanosb-runtime")
    return dev if File.exist?(dev)

    nil
  end

  # Locate the ensure-runtime.sh provisioner, in order of preference.
  def self.find_provisioner
    env = ENV["NANOSB_PROVISIONER"]
    return env if env && File.exist?(env)

    here = File.expand_path(__dir__)
    parent = here
    while parent != File.dirname(parent)
      cand = File.join(parent, "scripts", "sdk", "ensure-runtime.sh")
      return cand if File.exist?(cand)

      parent = File.dirname(parent)
    end

    side = File.join(here, "ensure-runtime.sh")
    return side if File.exist?(side)

    nil
  end

  def self.run_provisioner
    script = find_provisioner
    if script
      system("bash", script)
      raise "nanosb-runtime provisioner failed (#{script})" unless $?.success?
      return
    end

    Tempfile.create(["nanosandbox-ensure-", ".sh"]) do |f|
      f.write(EMBEDDED_PROVISIONER)
      f.flush
      system("bash", f.path)
      raise "nanosb-runtime provisioner failed" unless $?.success?
    end
  end

  # Return the helper path, provisioning it (download+verify) if missing.
  def self.ensure_runtime
    found = find_runtime_bin
    return found if found

    run_provisioner
    find_runtime_bin
  end

  # A running nanosb-runtime helper on a private socket.
  class ManagedRuntime
    attr_reader :dir, :socket, :pid

    def initialize(binary: nil, wait_secs: 15)
      @binary = binary || Nanosandbox.ensure_runtime
      unless @binary
        raise "nanosb-runtime not found and could not be provisioned; set " \
              "NANOSB_RUNTIME_BIN, or NANOSB_VERSION to allow auto-download"
      end

      @dir = Dir.mktmpdir("nanosb-e2e-")
      @socket = File.join(@dir, "helper.sock")
      @pid = spawn(@binary, "serve", "--socket", @socket, out: File::NULL, err: File::NULL)
      deadline = Time.now + wait_secs
      until File.exist?(@socket)
        raise "timed out waiting for the helper socket" if Time.now > deadline
        raise "nanosb-runtime exited before opening its socket" unless alive?

        sleep 0.05
      end
    end

    def alive?
      Process.kill(0, @pid)
      true
    rescue Errno::ESRCH, Errno::EPERM
      false
    end

    def close
      begin
        Process.kill("TERM", @pid)
        Process.wait(@pid)
      rescue StandardError
        begin
          Process.kill("KILL", @pid)
        rescue StandardError
          nil
        end
      end
      FileUtils.remove_entry(@dir) if @dir && File.directory?(@dir)
    end

    def self.open(**kwargs)
      rt = new(**kwargs)
      return rt unless block_given?

      begin
        yield rt
      ensure
        rt.close
      end
    end
  end
end