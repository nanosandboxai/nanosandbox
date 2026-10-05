# Zero-Image-Customization Security Checklist

Date: 2026-10-05
Branch: `feat/zero-image-customization`

Status legend: **Implemented** / **Partial** / **Deferred** (with rationale).

| # | Requirement | Status | Notes |
|---|-------------|--------|-------|
| 1 | VMM + supervisor run unprivileged | Implemented | libkrun uses HVF without root; gvproxy and the per-sandbox supervisor run as the invoking user. A dedicated UID is deferred (single-user macOS). |
| 2 | Dedicated APFS volume for virtiofs shares | Deferred | Shares live under `~/.nanosandbox/sandboxes/{name}/` and the user's project path. Residual risk documented below. |
| 3 | Config mounts read-only; workspace/state scoped per sandbox | Implemented | The mount planner marks agent config dirs read-only; workspace/state are RW and scoped under the sandbox dir (verified via `nanosb describe`). |
| 4 | Image digest pinning + cosign verification | Deferred | `ImageManager` pulls by tag; digest pinning and signature verification are follow-ups. |
| 5 | Console output rendered through a VT parser; no raw TTY passthrough | Partial | The TUI renders via `vt100`; `nanosb logs` prints raw bytes to stdout (no host TTY injection). Full OSC-sequence sanitization is a follow-up. |
| 6 | Secrets never persisted; never in cmdline | Partial | The supervisor does not write secrets; `sandbox.yml` env (which may contain keys) is stored in `config.json`, now written 0600 inside a 0700 sandbox dir. Dedicated secret delivery (SSH/vsock) is deferred. |
| 7 | TSI disabled; gvproxy only; forwards opt-in loopback-only | Implemented | Next-mode boots always use gvproxy (virtio-net); no port forwards by default. |
| 8 | No host path shares beyond declared mounts | Implemented | Only planner-declared virtiofs shares are registered with libkrun. |
| 9 | Control sockets 0700 dir / 0600 socket; peer same-UID | Implemented | The supervisor sets 0700 on the sandbox dir and 0600 on `control.sock`; peer-credential check on Linux, permission-based on macOS. |
| 10 | Guest hardening: rlimits, no devices, minimal cmdline | Partial | Next mode boots via libkrun without applying OCI caps/seccomp; the VM boundary and an ephemeral rootfs are the primary controls. |
| 11 | Logs 0600 | Implemented | `console.log` and `supervisor.log` are 0600 inside the 0700 sandbox dir. |
| 12 | CI: `cargo audit` | Deferred | Not yet wired into CI. |

## Residual risks (ranked)

1. **virtiofs host-path exposure** — libkrun does not confine the guest to the shared directory; mitigated by per-sandbox directories, not eliminated. Dedicated-volume isolation is deferred.
2. **Untrusted image as workload** — mitigated by the VM boundary; digest pinning/signature verification is deferred.
3. **Secrets in `sandbox.yml` env persisted in `config.json`** — mitigated by 0600/0700 permissions on the sandbox directory.
4. **Raw console bytes via `nanosb logs`** — the TUI uses a VT parser; the CLI writes plain stdout.

## Evidence

- Patched libkrun (v1.19.5 + next-mode init patch) verified on macOS Apple Silicon (HVF).
- `next_mode_test` (console, extra mounts, network) passing.
- `nanosb run --next` boots a vanilla image detached with console log capture.
- `nanosb apply/restart/describe/prune` lifecycle verified against a vanilla `alpine` image.
