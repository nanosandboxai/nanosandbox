# Workspace Isolation — Target Design (S5)

Status: **PARTIALLY IMPLEMENTED** (branch `feat/workspace-isolation`).
Shipped + tested: `gitcmd` hardening (#10), `nanosb logs` escape stripping (#13),
`~/.nanosandbox` mount verified per-sandbox (#14), `.nanosb-state` removal (#3),
macOS Seatbelt confinement of the VM subprocess (#2, denylist — verified boot +
credential deny), `/upload` + `/paste-image` removal + Ctrl/Cmd+V consolidation
(#5). Remaining (larger): `workspace.mode` flag (#1), `/discard`-in-guest (#7),
remove `/edit`/`cleanup`/`sanitize` (#8, gated on #9), host review repo (#9),
rootfs CoW (#11), quotas (#12), libkrun virtiofs patch (#15), Linux Landlock.
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
| `/upload`, `/paste-image` | host writes clone `.uploads/` | **Remove both** → Ctrl/Cmd+V handles image + host-path paste over the exec channel (never a workspace write) | removes host write into agent-writable state; one paste entry point |
| `/discard` | host `git reset --hard` on clone | **Redesign** → in-guest reset via exec, or re-seed staging | removes host write into agent-writable state |
| `/edit` | host tools write clone | **Remove** (or copy-out to a review dir first) | fundamentally conflicts with the model |
| `cleanup` auto-commit | host `git add/commit` on clone | **Remove** (or move in-guest) | explicit review/apply covers it |
| `sanitize_clone_config` | host writes `.git/config` | **Remove** once host stops running git on the clone | no longer needed |
| Session state `/workspace/.nanosb-state/` | legacy; host reads stale path | **Remove** — state is already in dedicated RW mounts (`<sandbox>/state/...`); point resume detection there | removes host trust in workspace state + fixes a latent resume bug |
| RO config mounts | guest-side `MS_RDONLY` only | **Document as non-security** | no host-side enforcement |
| `~/.nanosandbox` mount | RW | **Verify** it is `<sandbox>/state/nanosandbox`, never the global dir | global holds other sandboxes' state + firmware |

**Net:** remove or redesign 5 host-write features; 5 read-only features survive;
1 state location moves. This is the "remove features if they conflict with the
secure model" the design calls for.

---

## 4. Patch-libkrun feasibility (Phase 5, defense-in-depth)Implement per-component resolution in libkrun's virtualfs server: `openat(parent,
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

**Exact location (verified).** libkrun's virtiofs already uses `O_NOFOLLOW` on
`openat` in the primary paths, but `open_inode`
(`~/.cache/nanosandbox/libkrun/src/devices/src/virtio/fs/macos/passthrough.rs:744`)
reopens by a **stored path string** and **clears `O_NOFOLLOW`** (line 769:
`(flags | O_CLOEXEC) & (!O_NOFOLLOW) & (!O_EXLOCK)`) — the CVE-2026-77179
("path-string reopen") class. The correct fix is to reopen via a **held fd**
(fd-based `InodeHandle`, `openat(parent_fd, name, O_NOFOLLOW)`), not a path
string. This is a multi-day upstream change; documented, not rushed.

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

---

## 9. Linux solution (parallel to macOS)

libkrun's virtiofs is the **same in-process, unconfined code** on Linux, but Linux
gives us stronger, unprivileged, per-process confinement primitives — so the Linux
answer is cleaner than macOS (which is stuck with Seatbelt):

1. **Landlock (Linux 5.13+, unprivileged)** — the Linux analogue of Seatbelt.
   Apply a Landlock ruleset to the `internal-boot-vm` process allowing file access
   only under the staging tree + rootfs + firmware + sockets; everything else is
   denied by the kernel. No root, no namespaces needed. This is the primary
   Linux control.
2. **User + mount namespace** (`unshare -r --map-auto`, `pivot_root`) — belt-and-
   braces; run the VMM in a private mount namespace so the share is the only
   visible path. (virtiofsd's own `--sandbox namespace` model.)
3. **`openat2(RESOLVE_IN_ROOT | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS)` in
   the server** — the *correct* per-operation fix, available on Linux only (no
   macOS equivalent). Fold into the Phase 5 libkrun patch; on Linux this can fully
   eliminate the TOCTOU class that macOS cannot.
4. **Per-sandbox UID** — run the VMM/filesystem server as a dedicated UID with the
   staging tree chmod 0700; a real boundary on Linux (no macOS equivalent).

**Net:** Linux confinement = **Landlock + namespaces + `openat2`**; macOS =
**Seatbelt only** (weaker, no atomic path resolution). The architecture
(VM-private staging + host review repo + no host-trusted paths) is identical on
both; only the confinement primitive differs.

---

## 10. `/workspace/.nanosb-state/` — remove it

**It is legacy and not needed.** Agent session state today lives in the
**dedicated per-agent RW state mounts** (`mount_planner.rs:61-96`):
`/home/developer/.claude` → `<sandbox>/state/...`, `/home/developer/.codex`,
`/home/developer/.config/goose`, etc., with `HOME=/home/developer`. The plan doc
confirms the migration: *"State persistence: symlinks into `/workspace/.nanosb-state`
→ dedicated RW state mounts per sandbox."*

The resume detector still reads the **stale** location: `detect_agent_session_id_from_state`
(`src/tui/run.rs:5033`) reads `<clone>/.nanosb-state/<agent>/sessions`, called with
the **clone** path (`run.rs:5190, 5386`). `ensure_nanosb_state_gitignored`
(`project.rs:393`) only adds it to `.git/info/exclude`; nothing writes it.

**Action:**
- Remove `.nanosb-state` handling (`ensure_nanosb_state_gitignored`, the
  `.nanosb-state` reads) and point resume detection at the **dedicated state
  mount** (`<sandbox_dir>/state/...`), which the host already owns and which is
  **outside** the untrusted workspace.
- Benefit: the host no longer reads agent-writable workspace state (removes a
  trust coupling and a latent resume bug), and the workspace can be fully private.

---

## 11. Copy/paste vs `/upload` `/paste-image` — consolidate onto Ctrl/Cmd+V

What the code already does:
- **Text paste** — bracketed paste is forwarded to the guest terminal
  (`run.rs:580` `handle_paste_event`) → **already direct, no command needed**.
- **Ctrl/Cmd+V image** — the key handler reads the clipboard image and uploads it
  (`run.rs:1591-1601`, via `spawn_bytes_upload`). Image paste is **already
  keybinding-driven**; `/paste-image` (`run.rs:2551`) is a redundant alias.
- **Ctrl/Cmd+V with a host file path** — `detect_file_paths` (`upload.rs`) already
  detects absolute existing file paths in pasted text, but it is **test-only
  (dead in production)**. Wiring it makes path-paste a file transfer.

**Decision — remove BOTH `/upload` and `/paste-image`; make Ctrl/Cmd+V the single
paste/upload entry point.** On Ctrl/Cmd+V:
1. If the clipboard has an **image** → upload it (via the secure channel).
2. Else if the clipboard text contains **host file path(s)** (`detect_file_paths`)
   → upload them (via the secure channel) **and** also paste the text, with a
   "Uploaded X" notification.
3. Else → forward the text as a **bracketed paste** to the panel session (today's
   behavior).

Transport under the secure model: uploads go over the **exec channel** (vsock) to a
guest path (`/workspace/.uploads/` or a dedicated upload dir), **never** as a host
write into the workspace clone (Phase 3).

**Caveat:** auto-uploading any pasted absolute path can surprise (text that merely
*looks* like a path). Guard: only upload when the path exists **and** is a regular
file; show a notification; consider requiring the panel to be focused. This is a
UX judgement call to confirm.

## 12. Consolidated change list (this plan, all workstreams)

Everything the S5 work touches, in one place. **Bold = security-critical.**

| # | Change | Files | Phase |
|---|---|---|---|
| 1 | `workspace.mode: isolated \| shared` flag (isolated default) | `config`, `deploy_plan_for`, `mount_planner` | P0 |
| 2 | **Seatbelt (macOS) / Landlock+ns (Linux) confinement of `internal-boot-vm`** | `runtime/.../libkrun.rs` (`handle_boot_vm_subprocess`), new profile builder | P2 |
| 3 | **Remove `.nanosb-state`**; point resume detection at the dedicated state mount | `project.rs` (`ensure_nanosb_state_gitignored`), `run.rs` (`detect_agent_session_id_from_state`, callers 5190/5386) | P1 |
| 4 | Move any remaining agent session state to dedicated RW mounts | `deploy/mount_planner.rs` | P1 |
| 5 | **`/upload` + `/paste-image` removed**; Ctrl/Cmd+V consolidates image + host-path paste | `commands.rs`, `run.rs`, `upload.rs` (wire `detect_file_paths`), help/tests | P3 |
| 6 | Uploads over the **exec channel** (never a workspace write) | `upload.rs` + exec client | P3 |
| 7 | `/discard` → in-guest reset or re-seed (no host git on clone) | `run.rs` | P3 |
| 8 | Remove `/edit`; remove `cleanup` auto-commit; remove `sanitize_clone_config` | `run.rs`, `main.rs`, `gitcmd.rs` | P3 |
| 9 | **Host review repo**: guest bundle/patch → host applies; `/diff`/`/status`/`/apply` read it | `project.rs`, `run.rs`, `app.rs` | P4 |
| 10 | `gitcmd` clean env + strip `credential/merge/mergetool/difftool/gpg/pager/submodule/url/protocol` | `gitcmd.rs` | P0 |
| 11 | Rootfs: fresh/CoW per boot (avoid host-rootfs persistence) | `runtime` | P4 |
| 12 | Quota staging dir + inode cap + virtiofs queue cap | `runtime`/`config` | P4 |
| 13 | Console-output escape sanitization; host bounds exec frames | `renderer`, `terminal.rs` | P0 |
| 14 | Verify `~/.nanosandbox` mount is per-sandbox, never global | `mount_planner.rs` | P0 |
| 15 | Patch libkrun virtiofs: per-component `O_NOFOLLOW` + inode lineage (Linux `openat2`) | `runtime/scripts/patches/` | P5 |

### Phase plan

- **P0 (quick)** — flag (#1), `gitcmd` hardening (#10), console/frame sanitization (#13), `~/.nanosandbox` scope check (#14). No feature loss.
- **P1 (short)** — remove `.nanosb-state` (#3), state to dedicated mounts (#4). Fixes a latent resume bug.
- **P2 (medium) — the security win** — Seatbelt/Landlock confinement (#2) + the 10-vector escape tests. No feature change.
- **P3 (medium)** — remove `/upload` `/paste-image` + Ctrl/Cmd+V consolidation (#5), exec-channel uploads (#6), `/discard` redesign (#7), remove `/edit`/`cleanup`/`sanitize` (#8).
- **P4 (medium/large)** — host review repo (#9), rootfs CoW (#11), quotas (#12).
- **P5 (large, optional)** — libkrun virtiofs patch (#15).

### Commands before → after

```
before: /upload /paste-image /edit /discard /sync /diff /status /gc /disk ...
after : (Ctrl/Cmd+V: image | host-path | text)  /discard  /sync  /diff  /status  /apply  /gc  /disk ...
removed: /upload  /paste-image  /edit
redesigned: /discard (in-guest), /sync+/apply (host review repo)
```

---

## 13. Detailed summary

**Problem.** The agent is untrusted; the host project clone is shared RW into it
via libkrun's **in-process, unconfined** virtiofs, and the host then runs git on
that agent-writable clone. A symlink/`..`/hardlink/TOCTOU escape reaches host
paths outside the share (the exact class of CVE-2026-77179 / CVE-2026-47243).

**Hard constraints (verified).** (a) `krun_add_virtiofs` has no host-side
read-only, so "RO-share" is not a boundary; (b) libkrun's virtiofs runs in the VMM
process with no chroot/seccomp/userns; (c) the guest root is itself a share of the
host rootfs dir; (d) `gitcmd` is mitigation, not a boundary.

**Target model (best practice).** *Confined VM-private staging + host review repo*:
the agent works in a VM-private staging tree (treated as untrusted); the VMM
subprocess is confined by **Seatbelt (macOS) / Landlock + namespaces (Linux)** so
the virtiofs server can only reach staging + rootfs + firmware + sockets; the host
never runs git/editors/uploads on agent content — a host-owned review repo receives
a guest bundle and `/diff` `/status` `/apply` operate there. This matches the
industry (E2B/Modal/Fly/gVisor never share host dirs) and preserves the shipped
review/apply commands.

**Why Seatbelt/Landlock alone is not enough.** They confine the VMM, not the
host-side nanosb process that *reads* agent-controlled data, and can't stop
malicious content persisted for later host reads. So confinement **and**
"never share host-trusted paths" are both required. macOS is the weak platform
(Seatbelt only, no atomic path resolution); Linux can be made strictly stronger
(Landlock + userns + `openat2(RESOLVE_IN_ROOT)`).

**Decisions folded in.** Remove `.nanosb-state` (agent state already lives in
dedicated RW mounts — also fixes a latent stale-path resume bug). Remove `/upload`
and `/paste-image`; Ctrl/Cmd+V becomes the single paste/upload entry point (image →
upload; pasted host path → upload; else → bracketed-paste text), with the secure
exec-channel transport. Remove `/edit`, `cleanup` auto-commit, `sanitize_clone_config`.

**Verification.** 10 escape vectors (symlink / `..` / absolute / hardlink / TOCTOU
/ git-RCE / host-symlink-follow / session-injection / resource-exhaustion / rootfs)
each must **fail**, via a guest-side harness over the exec channel, plus a host-side
negative control so the tests can actually fail.

**Effort.** P0–P2 = short–medium (the win, no feature loss); P3 = medium (feature
removals); P4 = medium–large; P5 = large.



