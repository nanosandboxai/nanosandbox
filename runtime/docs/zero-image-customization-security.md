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
| 5 | Console output rendered through a VT parser; no raw TTY passthrough | Partial | The TUI (console attach, no SSH) renders via `vt100`; `nanosb logs` prints raw bytes to stdout (no host TTY injection). Full OSC-sequence sanitization is a follow-up. |
| 6 | Secrets never persisted; never in cmdline | Implemented | User env/secrets are stripped from `config.json`/`deploy.json` and from the supervisor argv; they are delivered via the supervisor's process environment (`NANOSB_BOOT_ENV`) and cleared before the VM boots. Residual: a caller passing `-e KEY=value` exposes the value in their own CLI argv, and a guest command that prints a secret lands it in `console.log`. |
| 7 | TSI disabled; gvproxy only; forwards opt-in loopback-only | Implemented | Next-mode boots always use gvproxy (virtio-net); no port forwards by default. |
| 8 | No host path shares beyond declared mounts | Implemented | Only planner-declared virtiofs shares are registered with libkrun. |
| 9 | Control sockets 0700 dir / 0600 socket; peer same-UID | Implemented | The supervisor sets 0700 on the sandbox dir and 0600 on `control.sock`; peer-credential check on Linux, permission-based on macOS. |
| 10 | Guest hardening: rlimits, no devices, minimal cmdline | Partial | Next mode boots via libkrun without applying OCI caps/seccomp; the VM boundary and an ephemeral rootfs are the primary controls. |
| 11 | Logs 0600 | Implemented | `console.log` and `supervisor.log` are 0600 inside the 0700 sandbox dir. |
| 12 | CI: `cargo audit` | Deferred | Not yet wired into CI. |
| 13 | In-guest control server is **opt-in and minimal** | Implemented | Default next mode has no in-guest server (one console stream). `nanosb run --exec` optionally injects a minimal **exec-only** agent over a **virtio-vsock** channel (`~/.nanosandbox/sandboxes/<name>/exec.sock`, port 1024). It is exec-only (no config CRUD, no secrets store, no HTTP), runs as the configured guest user (non-root by default), and is not reachable over the guest network. The earlier init-relay design was rejected; this is the industry-standard dedicated-channel model (smolvm/microsandbox). |

## Residual risks (ranked)

1. **virtiofs host-path exposure** — libkrun does not confine the guest to the shared directory; mitigated by per-sandbox directories, not eliminated. Dedicated-volume isolation is deferred.
2. **Untrusted image as workload** — mitigated by the VM boundary; digest pinning/signature verification is deferred.
3. **Secrets in the caller's own CLI argv** — passing `-e KEY=value` exposes the value in the `nanosb` process argv; prefer `--env-file` or host env expansion (`-e KEY`). Secrets are no longer persisted by nanosb itself.
4. **Raw console bytes via `nanosb logs`** — the TUI uses a VT parser; the CLI writes plain stdout.
5. **Opt-in exec agent** — `--exec` adds an in-guest listener for host-driven command execution. Mitigated: vsock-only (not network-reachable), gated by the sandbox dir's 0700/0600 permissions, exec verbs only, runs as the configured (non-root) user. Enabling it is an explicit per-sandbox choice; it is off by default.

## Host↔agent git change-management hardening

The agent has a RW virtiofs share over its clone, so it can rewrite the clone's
`.git/config`/hooks/attributes. Earlier, the host ran plain `git` against that
clone, which made agent-controlled config a host-code-execution vector
(`core.fsmonitor`). This is now closed:

- **All host git calls** go through `src/tui/gitcmd.rs::host_git()`, which
  disables `core.fsmonitor`, `core.hooksPath`, `core.pager/editor/sshCommand/
  gitProxy/askPass/alternateRefsCommand`, `core.attributesFile`, `diff.external`,
  `uploadpack.packObjectsHook`, plus `--no-optional-locks`. Covers the sidebar
  status/diff, the sync poll, `/branches`, `/gitsync now`, and `cmd_cleanup`.
- **`.git` validation** — `has_real_git_dir()` refuses a symlinked or escaping
  `.git`, so a compromised clone cannot redirect host git into another repo.
- **Source repo is read-only to nanosb** — the setup-time source branch was
  removed; agent changes are fetched to `refs/nanosb/<id>` (never
  `refs/heads/*`), and the user reviews (`/diff`, `/status`) and applies
  explicitly.
- **Non-git projects** are no longer `git init`-ed in place: the repo is
  initialised inside the clone and the source directory is left untouched.
- **`/edit`** sanitizes the clone's `.git/config` before opening it in a
  git-aware tool.
- **`--env-file`** rejects symlinks/non-regular files and caps size before read.
- **`nanosb gc`** reclaims dead supervisor dirs and clone trees not referenced
  by a saved session; `nanosb cleanup` skips session-referenced clones and
  supports `--dry-run`.


## Evidence

- Patched libkrun (v1.19.5 + next-mode init patch) verified on macOS Apple Silicon (HVF).
- `next_mode_test` (console, extra mounts, network) passing.
- `nanosb run --next` boots a vanilla image detached with console log capture.
- `nanosb apply/restart/describe/prune` lifecycle verified against a vanilla `alpine` image.
- TUI attaches panels via the supervisor console stream; the in-VM agent-gateway (HTTP/SSE) and SSH transports are removed from `src/tui/**` (no `russh`/`connect_ssh`/`gateway()` references).
- Host-side exec over virtio-vsock verified end-to-end: `nanosb run --exec` boots the agent, `nanosb exec <sb> -- echo OK` returns output and exit codes (exit 7/5 propagated). The spike (`runtime/crates/runtime/examples/spike-vsock-exec.rs`) proves the transport.
