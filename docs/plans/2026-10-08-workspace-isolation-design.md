# Workspace Isolation — Target Design (S5)

Status: **DESIGN for review** (no code changed)
Date: 2026-10-08
Scope: how the host project workspace is exposed to an untrusted agent microVM.
Companion: `docs/plans/2026-10-08-project-management-and-isolation-options.md`.

---

## 0. Problem

The agent runs untrusted code in a libkrun microVM. Today the host project **clone**
(`~/.nanosandbox/clones/<hash>/<id>`) is shared into the guest at `/workspace` via
**virtiofs**, and the host then runs `git` against that agent-writable clone. A
virtiofs escape (symlink / `..` / hardlink / TOCTOU) would let the guest reach
host paths outside the share — read `~/.ssh/id_rsa`, write `~/Library/LaunchAgents`,
etc. Active, current CVEs are this exact class:

- **CVE-2026-77179** (Docker Sandboxes on macOS) — virtio-fs TOCTOU symlink
  escalation via path-string reopen; CVSS 9.4.
- **CVE-2026-47243** (Kata) — raw `FUSE_SYMLINK` with an absolute host path; CVSS 9.2.

### Verified facts that reframe the fix

1. **`krun_add_virtiofs(ctx, tag, path)` has no read-only parameter.**
   `readonly` only reaches the guest as an `MS_RDONLY` mount flag in the init
   patch; **there is no host-side read-only enforcement** — an untrusted guest can
   remount RW. "RO-share" is therefore not a boundary.
2. **libkrun's virtiofs is in-process and unconfined.** It runs in the same
   process as the VMM (`internal-boot-vm`), uses `openat(O_NOFOLLOW)` only for the
   final component, and its `symlink()` handler calls `libc::symlinkat()` without
   validating the target. libkrun's own README: *"libkrun does not provide any
   protection against the guest attempting to access other directories… A mount
   point isolation mechanism from the host should be used."* There is no external
   virtiofsd to pass `--sandbox chroot/namespace` to; on macOS there is no user
   namespace and no chroot for the VMM.
3. **The guest root is itself a virtiofs share** of the host rootfs directory, so
   the in-process server serves the rootfs too and guest writes to `/` persist to
   the host rootfs dir.
4. **`gitcmd` is a mitigation, not a boundary.** It disables
   `core.fsmonitor/hooksPath/pager/editor/sshCommand/gitProxy/diff.external` but
   sets **no clean env** (`GIT_CONFIG_NOSYSTEM`, `GIT_CONFIG_GLOBAL`, `HOME`) and
   does not neutralize `core.worktree`, `credential.helper`, `merge.*`,
   `mergetool.*`, `difftool.*`, `gpg.*`, `pager.*`, `submodule.*`, `url.*`.
5. **Industry does not share host dirs.** E2B (Firecracker), Modal, Fly.io,
   Daytona, gVisor all copy/seed into the VM and sync back explicitly. Only
   microsandbox (libkrun) optionally RW-mounts a host dir, relying on the VM
   boundary. APFS volume boundaries are **not** security boundaries (symlinks
   cross them).

### Blast radius (11 features depend on the live RW share)

- **Write via host:** upload + paste-image (write `<clone>/.uploads/`),
  `cleanup` auto-commit (`git add/commit`), `/discard` (`git reset --hard`),
  `/edit` (external tools write the clone), `sanitize_clone_config`.
- **Read via host (fine on a RO mount):** sidebar `git status`/`git diff`,
  `/diff`, `/status`, `/sync` fetch, `/branches`.
- **In-guest writes:** agent files in `/workspace`; agent session state in
  `/workspace/.nanosb-state/` (which the **host reads** for resume detection).
- **Other RW mounts:** per-agent state dirs under `<sandbox>/state/` + the
  sandbox-scoped `~/.nanosandbox`.

---

## 1. Target architecture

**Chosen model — "Confined VM-private staging + host review repo":**

1. The agent works in a **VM-private staging tree** (today's clone) that is
   treated as **untrusted guest state**, never as a host-trusted path.
2. The `internal-boot-vm` subprocess runs under a **deny-default macOS Seatbelt
   profile** so the in-process virtiofs server can only reach the staging tree,
   the rootfs, the firmware libs, and the gvoss/vsock/log paths.
3. The host **never runs git/editors/uploads against agent-controlled content**.
   A **host review repo** (owned by the host, seeded from the source) receives a
   guest-produced `git bundle`/patch; `/diff`, `/status`, `/apply` operate there.

### Why this model (ranked)

| Rank | Model | Security | Cost | Verdict |
|---|---|---|---|---|
| 1 | In-guest workspace + vsock FS verbs (no host FS shared) | Strongest | Large | End-state / escalation trigger |
| **2** | **Copy-in/out + Seatbelt-confined VMM + host review repo** | **Strong; industry pattern** | **Medium** | **PRIMARY** |
| 3 | Seatbelt-confined VMM alone (share the clone) | Contains virtiofs escape; host still trusts agent content | Short | Necessary layer, not a model |
| 4 | Patched-libkrun `openat2`-equivalent | Fixes the server; maintenance burden; doesn't fix host-trust | Large | Defense-in-depth (Phase 5) |
| 5 | RO-share + scratch | **Broken** (no host-side RO; RO doesn't fix reads or host-trust) | — | Reject |

Model #2 fits what already exists (the clone *is* copy-in), preserves the shipped
review/apply commands, and closes both halves of the gap: Seatbelt contains the
virtiofs server, and the review repo removes host trust in agent `.git`.

**Escalation trigger → model #1** if multi-tenant, if Apple removes Seatbelt, or
if the libkrun patch proves unmaintainable.

---

## 2. macOS confinement — Seatbelt profile

Applied in the child at the top of `handle_boot_vm_subprocess`
(`runtime/crates/runtime/src/runtime/libkrun.rs`), after parsing `BootVmRequest`
(which carries every mount path) and before `preload_libkrunfw()`. Build the
profile dynamically from `rootfs_path`, `mounts`, `extra_mounts`,
`gvproxy_socket`, `vsock_socket`, and the console log path.

**Allow (deny-default otherwise):**

```
(version 1) (deny default)
(allow file-read-metadata)
(allow sysctl-read) (allow ipc-posix-shm)
(allow signal (target self))
(allow file-read*  (subpath "<rootfs>")) (allow file-write* (subpath "<rootfs>"))
(allow file-read*  (subpath "<each mount host_path>"))
(allow file-write* (subpath "<each RW mount host_path>"))
(allow file-read*  (subpath "~/.nanosandbox/libs"))            ; libkrunfw dylib
(allow file-read*  (subpath "/System/Library") (subpath "/usr/lib")
                    (literal "/dev/null") (literal "/dev/urandom") (literal "/dev/random"))
(allow file-write* (literal "<gvproxy.sock>") (literal "<vsock.sock>") (literal "<console.log>"))
(allow network-outbound (literal "<gvproxy.sock>"))
(allow mach-lookup (global-name "com.apple.system.logger")
  (global-name "com.apple.system.notification_center")
  (global-name "com.apple.system.opendirectoryd.libinfo")
  (global-name "com.apple.system.DirectoryService.libinfo_v1")
  (global-name "com.apple.bsd.dirhelper"))
```

**Deny:** all `file-read*`/`file-write*` outside the allowlist (blocks `~/.ssh`,
`~/.aws`, the source repo, `/etc/passwd`); `network*` except the gvproxy socket;
`process-exec`; `mach-lookup` outside the list.

**Is it enough alone? No.** Seatbelt is deprecated (profile bugs likely), confines
the **VMM** but not the host-side nanosb process that reads agent data, and does
not stop malicious content persisted into the rootfs/staging for the host to
read later. So: Seatbelt **and** "never share host-trusted paths" are both
required. On macOS you may share the **staging** dir under Seatbelt; never share
the real project or any host-trusted path.

---

## 3. Feature set — keep / redesign / remove

| Feature | Today | Target | Rationale |
|---|---|---|---|
| `/diff` `/status` `/sync` `/branches` `/apply` | host git on agent clone | **Keep** — read the **host review repo** (Phase 4); interim: keep `gitcmd` + clean env | review/apply model preserved |
| `/upload`, `/paste-image` | host writes clone `.uploads/` | **Redesign** → exec-channel write, or a dedicated upload mount (not the workspace) | removes host write into agent-writable state |
| `/discard` | host `git reset --hard` on clone | **Redesign** → in-guest reset via exec, or re-seed staging | removes host write into agent-writable state |
| `/edit` | host tools write clone | **Remove** (or copy-out to a review dir first) | fundamentally conflicts with the model |
| `cleanup` auto-commit | host `git add/commit` on clone | **Remove** (or move in-guest) | explicit review/apply covers it |
| `sanitize_clone_config` | host writes `.git/config` | **Remove** once host stops running git on the clone | no longer needed |
| Session state `/workspace/.nanosb-state/` | agent-writable, host-read | **Move** to a dedicated RW mount; host treats it as untrusted | removes host trust in workspace state |
| RO config mounts | guest-side `MS_RDONLY` only | **Document as non-security** | no host-side enforcement |
| `~/.nanosandbox` mount | RW | **Verify** it is `<sandbox>/state/nanosandbox`, never the global dir | global holds other sandboxes' state + firmware |

**Net:** remove or redesign 5 host-write features; 5 read-only features survive;
1 state location moves. This is the "remove features if they conflict with the
secure model" the design calls for.

---

## 4. Patch-libkrun feasibility (Phase 5, defense-in-depth)

Implement per-component resolution in libkrun's virtualfs server: `openat(parent,
comp, O_NOFOLLOW|O_PATH)` for every component, verify the resulting
`(st_dev, st_ino)` is a descendant of the share root (canonical-inode lineage),
reject absolute paths / `..`, and validate `symlink`/`linkat`/`renameat` targets
stay inside the share. Operate relative to held fds, never re-resolve by string.
This is the Docker 0.42 virtiofsd fix.

**Residual risk on macOS without `openat2`:** TOCTOU cannot be fully eliminated
(no atomic `RESOLVE_BENEATH`; a rename can swap a dir between component opens) —
mitigate by holding `O_PATH` fds for the whole resolution and using `*at`
syscalls on the final fd. Hardlinks are contained only if `linkat` sources are
always share-internal; a pre-existing hardlink in the seeded tree is exposed, so
seed with a copy that **breaks hardlinks** (`clonefile` may preserve them —
verify). Maintenance: must rebase on every libkrun upgrade.

Verdict: do it as **defense-in-depth**, not as the primary control.

---

## 5. Migration phases (non-breaking)

- **Phase 0 — Flag (`quick`).** Add `workspace.mode: isolated | shared` (default
  `isolated`). `shared` = today's behavior (explicit opt-in). No change yet.
- **Phase 1 — Session state out of the workspace (`short`).** Dedicated RW mount
  for `.nanosb-state`; host treats it as untrusted.
- **Phase 2 — Seatbelt confinement (`medium`).** Deny-default profile in
  `handle_boot_vm_subprocess`; negative-control tests. **Biggest win, zero
  feature change.**
- **Phase 3 — Redesign host-write features (`medium`).** Upload/paste via exec;
  `/discard` in-guest/re-seed; remove `/edit`; remove `cleanup` auto-commit.
- **Phase 4 — Host review repo (`medium/large`).** Guest produces a
  `git bundle`/patch over vsock; host applies it to a clean review repo;
  `/diff`/`/status`/`/apply` operate there. No host git on agent `.git`.
- **Phase 5 (optional) — Patch libkrun virtiofs (`large`)** or replace virtiofs
  with vsock FS verbs (`large`).

**Tests:** guest-side harness (run over the exec channel) attempts each escape
vector and asserts failure; host-side test asserts the profile is deny-default
and that **removing the confinement makes the negative control fail** (the test
is a real control, not a tautology).

---

## 6. Definition of done / acceptance (testable)

Each vector must **fail**, verified by a guest-side test run over the exec
channel, plus a host-side assertion:

1. Symlink escape (`evil -> ~/.ssh/id_rsa`) → denied.
2. `..` traversal (`../../../../etc/passwd`) → denied.
3. Absolute path `/etc/passwd` via the share → denied.
4. Hardlink escape (`linkat` a host file outside the share) → denied.
5. TOCTOU (race a rename/symlink swap) → denied.
6. Host git RCE (malicious `.git/config`: `fsmonitor`/`hooksPath`/
   `credential.helper`/`core.worktree`) + `/status` → no execution.
7. Host symlink follow (symlink in share + `/upload`/`/edit`/`cleanup`) → no
   write outside the share.
8. Session-state injection (malicious `.nanosb-state`) → no effect on host resume.
9. Resource exhaustion (unbounded file creation) → bounded by quota.
10. Rootfs persistence (guest write to `/`) → confined to the rootfs dir, not
    `~/.nanosandbox` siblings.

---

## 7. Gaps to fix regardless (cheap, do now)

- **`gitcmd` cleanliness:** set `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null`,
  `HOME=/nonexistent`; add `-c core.worktree=<clone>`; strip
  `[credential] [merge] [mergetool] [difftool] [gpg] [pager] [submodule] [url] [protocol]`.
- **Rootfs is a shared host path** — consider a fresh / CoW rootfs per boot.
- **`~/.nanosandbox` scope** — verify per-sandbox, never global.
- **Console log terminal-escape injection** — sanitize guest console output
  before the TUI renders it.
- **Exec-channel frame validation** — the host must bound/validate guest frames
  (the guest caps at 64 MB; the host should too).
- **Resource exhaustion** — quota the staging dir, cap inodes, cap virtiofs
  queue depth.

---

## 8. Effort

| Phase | Effort |
|---|---|
| 0–2 (the security win) | Short–Medium |
| 3 | Medium |
| 4 | Medium–Large |
| 5 | Large |
