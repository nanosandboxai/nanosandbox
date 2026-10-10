# Industry workspace isolation vs nanosandbox — comparison + S5 implications

Status: complete · Evidence: 8 parallel research workers (see `.omo/ulw-research/20261010-072223/`)
Date: 2026-10-10 · Scope: how sandbox products isolate the workspace + move files across guest/host, vs nanosandbox's libkrun-virtiofs-share + host-git-on-clone model.

> CVE identifiers below were reported by research workers with NVD/CVE.org/GHSA links;
> they were **not** independently re-verified against the live databases in this run. Treat
> severities as reported, not measured.

## 0. Verdict (one screen)

1. **The cloud norm is "never share a host directory into the VM."** E2B, Modal, Fly, Cloudflare, Vercel, Northflank, CodeSandbox all transfer files in/out via an explicit API (upload/download), `git clone` **inside** the guest, or a volume — never a host path. The host never runs tools on agent-written content.
2. **nanosandbox is *not* a total outlier** — it sits in the **local-first libkrun peer group** (microsandbox, agent-vm, BoxLite, iii-sandbox) where a host dir *is* bind-mounted via virtio-fs. But that whole group is explicit that **libkrun's virtiofs is not a security boundary** and must be wrapped in host-side isolation.
3. **The best-in-class local models already solve #9 the way we planned:** Docker Sandboxes **clone mode** (host repo RO, agent works on a private clone in the VM, host fetches via a localhost Git daemon) and **brood-box** (COW snapshot of the workspace → virtio-fs → agent edits the snapshot → host does a per-file diff + review → flush with hash re-verification). Both are the "agent writes a copy; host reviews and applies" pattern.
4. **2026 was a heavy virtiofs CVE year** (CVE-2026-77179 Docker-Sandboxes macOS symlink escape 9.4; CVE-2026-79994 UDS-relay TOCTOU 8.7; CVE-2026-47243 Kata raw-`FUSE_SYMLINK` 9.2; CVE-2026-44210 Kata arg-injection 9.9; CVE-2026-93827 kernel double-free 8.4). Our fork hardening (reject traversal names in `name_to_path`) maps to **Control 6** of the consensus control set.
5. **Our Seatbelt profile is the weakest link.** It is a **deny-list on top of `(allow default)`**, which security research (Pillar Security 2026) calls "not a sandbox." The correct posture is **deny-default + iterate**, granting only the specific network ops libkrun's virtio-net needs — the virtio-net breakage we hit is a profiling problem, not a reason to keep `(allow default)`.

## 1. How the cloud sandboxes isolate the workspace

| Product | Isolation | Workspace in/out | Host dir shared? | Host tools on agent files? |
|---|---|---|---|---|
| **E2B** | Firecracker microVM | SDK files API + git clone in-VM; snapshot export | **No** | No |
| **Modal** | gVisor (default) / Firecracker VM | `modal.Image` + `modal.Volume` + filesystem API | **No** | No |
| **Fly.io** | Firecracker on bare metal | container image + git clone; continuous disk sync | **No** | No |
| **Cloudflare** | V8 isolates (+ Containers) | code bundle + SQLite VFS + R2 mounts | **No** (no host FS access at all) | No |
| **Vercel Sandbox** | Firecracker | files API + git clone + snapshots | **No** | No |
| **Northflank** | Kata/Cloud-Hypervisor, gVisor (GPU) | image + exec API + network volumes | **No** | No |
| **CodeSandbox** | Firecracker | git-backed workspace + SDK archive sync | **No** | No |
| **Daytona** | Firecracker / container | OCI image + SDK fs ops + git | **No** (remote) | No |

Common shape: **private guest rootfs = CoW/overlay over a read-only template** (squashfs+ext4, EROFS layers, ext4 snapshot, reflink clone); **files cross the boundary only through an explicit broker** (SDK API, guest agent over vsock, git daemon), and **git runs inside the guest**.

## 2. The local-first libkrun peer group (where nanosandbox actually sits)

| Project | VMM | Host dir → guest | Writable? | Review gate? |
|---|---|---|---|---|
| **microsandbox** | libkrun | virtio-fs bind at `/workspace`; host-side `openat2(RESOLVE_BENEATH)` containment | Yes | No |
| **agent-vm** | libkrun (via microsandbox) | virtio-fs bind at host path | Yes | No |
| **BoxLite** | libkrun | virtio-fs volumes | Yes | No |
| **iii-sandbox** | libkrun | **no bind** — upload API only | n/a | No |
| **brood-box (bbox)** | libkrun (go-microvm) | **COW snapshot** (FICLONE/clonefile) mounted at `/workspace` | Snapshot only | **Yes** (per-file diff + hash re-verify) |
| **Docker Sandboxes** | custom microVM | **clone mode**: host repo **read-only**, private clone in VM, localhost Git daemon | Clone only (clone mode) | **Yes** (fetch from `sandbox-<id>` remote) |
| **nanosandbox (today)** | libkrun | host **clone** bind-mounted RW at `/workspace` | Yes | host git on the clone (`/diff`,`/sync`,`/apply`) |

**Read:** our model is the peer-group norm *minus* the host-side containment the peers added and *minus* the review gate the two best peers added. The gap is not "we share a dir" — it's (a) we don't contain the sharing host-side, and (b) we let host git touch agent-writable state.

## 3. The 2026 virtiofs/9p vulnerability wave (reported)

| CVE (reported) | Product | Class | CVSS | Our exposure |
|---|---|---|---|---|
| CVE-2026-77179 | Docker Sandboxes macOS | cached **path-string reopen** follows swapped symlink | 9.4 | macOS libkrun uses `/.vol/{dev}/{ino}` **inode-addressed** reopen + component-wise `openat(O_NOFOLLOW)` → **not** the path-string class |
| CVE-2026-79994 | Docker Sandboxes | UDS relay TOCTOU (validate path, reconnect by path) | 8.7 | our exec relay — needs the fd-based check |
| CVE-2026-47243 | Kata virtiofsd | raw `FUSE_SYMLINK` with absolute host path | 9.2 | **addressed** by our `name_to_path` traversal rejection (fork `a148cff4`) |
| CVE-2026-44210 | Kata virtiofsd | arg injection → serve `/` | 9.9 | n/a (no annotation surface) |
| CVE-2026-93827 | Linux kernel | `virtio_fs_setup_vqs` double-free | 8.4 | n/a on macOS |

Consensus control set (all sources): **[1] deny-default Seatbelt** (macOS has no mount namespaces, so this *is* our mount isolation); **[2] descriptor-relative resolution** from a pinned export fd; **[3] `O_NOFOLLOW` on every component**; **[4] reject traversal names**; **[5] pin FDs — never re-resolve by path**; **[6] validate `FUSE_SYMLINK`/`FUSE_LINK` targets**; **[7] self-test both sides of every policy rule in CI**; **[8] `openat2(RESOLVE_BENEATH)` on Linux**.

## 4. Cross-cutting patterns worth adopting

- **Trust-handoff flaw** (Pillar Security 2026): the sandbox governs the agent's *actions*, not the downstream *effects* of files it writes that an unsandboxed component later executes (hook configs, `.git/config`, virtualenv interpreters). → directly motivates #9 + #8.
- **Protected paths are universal**: `.git/` (esp. `hooks/`, `config`), agent config dirs, shell rc files, `.env`/`~/.ssh`/`~/.aws` are always write-protected even in RW modes. (Claude Code, Codex, Cursor, Docker Sandboxes.)
- **Git worktree per run** is the emerging git-layer isolation (Docker Sandboxes `--branch`, Watchfire, WorktreePilot, Claude Code `--isolation worktree`).
- **Credential isolation** (proxy injection / placeholder substitution) is treated as mandatory; secrets never enter the VM (microsandbox, agent-vm, Docker Sandboxes, Cursor, Claude Code cloud).
- **No platform publicly documents inode caps** — a genuine gap; Firecracker offers per-drive rate limiter, jailer `fsize`/`no-file`, cgroup `pids.max`; inode caps need guest ext4/XFS quota.

## 5. Implications per remaining S5 item

| # | Item | Verdict from the evidence | Action |
|---|---|---|---|
| **1** | `workspace.mode: isolated\|shared` flag | peers expose direct vs clone/COW modes as a first-class switch (Docker `--clone`, brood-box `--workspace-mode=direct`) | add flag, default **isolated** |
| **6** | exec-channel uploads | cloud norm is an explicit broker, not a workspace write | implement uploads over the exec/vsock channel |
| **7** | `/discard` in-guest | peers never mutate host state from in-guest ops | move `discard` in-guest (reset the VM copy) |
| **8** | remove `/edit`; `cleanup` auto-commit; `sanitize_clone_config` | trust-handoff flaw: any host tool on agent files is the escape | remove all three |
| **9** | **host review repo** | **the two best local peers implement exactly this** (Docker clone-mode Git daemon; brood-box COW+diff-review) | implement: host repo **RO** + private clone in VM + explicit fetch/apply |
| **11** | rootfs fresh/CoW per boot | universal: overlay/reflink/EROFS per sandbox; reflink is cheapest | adopt reflink/`clonefile` CoW rootfs (fallback sparse copy) |
| **12** | quotas | no peer documents inode caps; Firecracker = rate-limiter + rlimits + cgroups | add drive rate-limiter + `pids.max` + guest ext4 quota for inodes |
| — | Linux confinement | peers that run on Linux use `openat2(RESOLVE_BENEATH)` + namespaces | `openat2` in the fork's Linux passthrough |
| **2'** | **Seatbelt posture** (re-opens #2) | deny-list on `(allow default)` is "not a sandbox" | convert to **deny-default**, iteratively grant virtio-net ops, add a policy self-test |

## 6. Honest bottom line

Sharing a host directory RW into the guest is acceptable **for local-first libkrun tools** — provided it is (a) contained host-side and (b) not followed by host tools executing agent-written state. We have (a) partially (Seatbelt — but weak posture, and the fork's inode-addressed reopen + name hardening) and (b) not at all (host git runs on the agent-writable clone). The single highest-value S5 change is **#9 host review repo** plus **#2' deny-default Seatbelt**; the rest (#1/#6/#7/#8/#11/#12) are the supporting moves that make the isolated mode coherent.
