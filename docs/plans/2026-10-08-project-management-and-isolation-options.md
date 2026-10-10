# Project Isolation & Management — Options Analysis

Status: **IMPLEMENTED** (decisions 1–5 applied; worktrees rejected; explicit
review/apply model; project registry + `/project` switch; reference-aware
`cleanup --dry-run`; `nanosb gc`). Virtiofs confinement (§4/§A4) remains a
platform-level deferral gating multi-tenant.
Date: 2026-10-08
Companion to: `docs/plans/2026-10-08-tui-command-security-refactor-plan.md`
Question driving this: *can we improve project management + the host↔agent change model — securely, with low complexity and low breakage risk?*

---

## 1. `git worktree` per agent — verdict: **NO** (already rejected in-repo)

**In-repo rationale** (`sandbox/crates/sandbox/src/project.rs:7-10`):
> We use `git clone --local` instead of `git worktree` because worktrees create a `.git` FILE containing an absolute host path (`gitdir: /host/path/...`) which doesn't resolve inside a VM mounted via VirtioFS. Local clones create a proper `.git` directory that works in any filesystem namespace.

Two independent reasons it's wrong here:

**(a) It breaks in the VM.** A linked worktree's `.git` is a *file* pointing at `<main-repo>/.git/worktrees/<id>` by absolute host path — the guest can't resolve it, and it leaks host paths into the guest.

**(b) It's *less* secure for an untrusted agent.** A worktree shares the **main repo's** object store, refs, hooks, and `.git/worktrees/<id>/` metadata. An agent with RW access to its worktree can influence the **main repo** (write `.git/hooks`, corrupt refs/index, inject `include.path`/`core.hooksPath` in config). That deepens the very trust coupling we're trying to remove. A `git clone --local` has its **own** `.git`; the source repo's `.git` is never mounted.

**Conclusion:** keep clones. Worktrees solve "many working trees, one repo" — a *convenience* problem — not a *trust-boundary* problem, and they regress ours.

---

## 2. How other products handle host↔sandbox change flow (landscape)

Pattern across mature products: **the source tree is the boundary; changes are explicit; the untrusted agent never drives host git.**

| Product | Workspace in sandbox | Change return | Host git driven by agent? |
|---|---|---|---|
| **microsandbox** | mount/volume | explicit (you handle files) | no |
| **e2b** | filesystem API (upload/download) | explicit copy via API | no |
| **Daytona** | `daytona` mounts project into devcontainer | git is the user's; explicit | no |
| **Modal** | ephemeral FS + explicit volumes | explicit (volumes/artifacts) | no |
| **Runloop** | devbox, explicit file API | explicit | no |
| **GitHub Codespaces / devcontainers** | **bind-mount** the repo into the container | you commit/push yourself | no (container has no special host git) |
| **Cursor / Claude Code cloud agents** | cloud clone of the repo | **you review + merge a branch/PR** | no |

**The common design:** sandbox gets a copy/clone (or a mounted working dir); the agent works there; the **user** reviews a diff and merges/applies. No product auto-ingests agent-written git state into the host repo — which is exactly what nanosandbox does today (the S1–S3 class). nanosandbox's git-sync is *more* magical than the industry norm and *more* risky.

**Recommendation:** move nanosandbox toward the norm — a clear diff/review/apply step; keep the auto-sync *opt-in and safe* (namespaced refs, FF-only, hardened git).

---

## 3. The low-complexity, low-breakage target design

Three changes, none of which rearchitect the isolation model:

### 3.1 Harden every host git call (kills S1/S2 + the `cleanup` variant)
- One helper (`gitcmd::host_git()`) with `-c core.fsmonitor=false -c core.hooksPath=/dev/null -c core.pager=cat -c core.editor=false -c core.sshCommand=false -c core.gitProxy= -c core.askPass= -c core.alternateRefsCommand= -c core.attributesFile=/dev/null -c diff.external= --no-optional-locks`.
- Replace **all** `Command::new("git")` in `src/tui/app.rs` (610,652,695,724,754), `src/tui/run.rs` (2187,2313) **and `src/main.rs cmd_cleanup` (2252+)**.
- Guard with `gitcmd::has_real_git_dir()` (reject symlinked `.git`).

### 3.2 Stop writing into the source repo (kills S3)
- Don't create branches in the source at setup (`project.rs:280`).
- Fetch into `refs/nanosb/<id>` only; apply explicitly, **fast-forward-only**.

### 3.3 Surface review/apply in the TUI with clear commands
- `/diff`, `/status`, `/sync` (dry-run) , `/apply`. Familiar, low-magic, matches the industry model.

**Complexity:** ~2 helpers + call-site swaps + 2–4 commands. **Breakage risk:** low (defaults unchanged; auto-sync stays opt-in).

---

## 4. Project management — gaps & improvements

Verified surfaces: `cmd_cleanup`, `cmd_sessions`, `sandbox::project::clones_dir`, `Session`.

| # | Gap | Evidence | Improvement |
|---|---|---|---|
| P1 | **No project registry** — projects are implicit (CWD / `--project`); no list of known projects | `main.rs` global flags; TUI takes one project | `nanosb projects` list + TUI "recent projects" picker; store `~/.nanosandbox/projects.json` |
| P2 | **Single-project TUI** — one project per process; no switching | `run_tui(project_path, …)` | Allow panels from multiple projects, or a `/project switch` (bigger) |
| P3 | **`cleanup` is a blunt instrument** — removes **all** clones for a project (not just stale), after auto-committing | `main.rs:2252-2300` | Make it remove only **unreferenced** clones (cross-check sessions); add `--dry-run` |
| P4 | **`cleanup` is also an RCE surface** — unhardened `git status/add/commit` in clones | `main.rs:2268-2300` | Route through `gitcmd::host_git()` (see §3.1) |
| P5 | **Session↔clone coupling** — cleaning clones breaks resume; deleting sessions orphans clones | `clones_dir` keyed by path hash; `Session` keyed by path hash | Reconcile: resume tolerates missing clone; `gc` removes clones with no session AND no running sandbox |
| P6 | **Accumulation / no unified GC** — clones, bundles, sessions, supervisor dirs, sockets, logs | observed orphaned gvproxy/bundles during QA | One `nanosb gc` (and a TUI "disk usage" view): remove dead supervisor dirs/sockets, orphan bundles, unreferenced clones, expired sessions/logs |
| P7 | **Stale supervisor state** — stopped/errored supervisor dirs + `control.sock` linger | `SupervisorClient`, `ps` reads dirs | GC + `/doctor`-style TUI reconcile |
| P8 | **No auto project registration on TUI launch** | `run_tui` | Register CWD project + last-used on start; show in picker |

**Suggested TUI additions (project management):** `/projects` (list/switch), `/gc` (dry-run + confirm), `/disk` (usage). **CLI:** `nanosb projects`, `nanosb gc [--dry-run]`, tighten `cleanup` semantics.

---

## 5. Priority vs the security plan

Fold into the existing plan:
- **P0 (security, must-fix):** §3.1 hardening (incl. `cmd_cleanup`), §3.2 no source writes, S6 `/edit`, S7 `--env-file`.
- **P1:** `/diff` + `/status` + `/sync`/`/apply` (§3.3), `cleanup` hardening + `--dry-run` (P3/P4).
- **P2:** `nanosb gc` + TUI `/gc` `/disk` (P5/P6/P7), tolerate-missing-clone on resume.
- **P3:** project registry + picker + multi-project (P1/P2/P8) — the biggest, discuss separately.

---

## 6. Decisions to discuss
1. Adopt the **industry-normal explicit review/apply** model (keep clone isolation; stop auto-writing source)? *recommend yes.*
2. Confirm **worktrees are out** (per in-repo rationale + shared-repo trust). *recommend yes.*
3. Project registry + picker now, or defer? (P1/P8 cheap; P2 multi-project is the big one.)
4. `cleanup` semantics: reference-aware + `--dry-run` — OK?
5. Unified `nanosb gc` — in scope now or follow-up?
