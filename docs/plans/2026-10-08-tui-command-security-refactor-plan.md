# TUI Command Surface & Change-Management Security Refactor — Plan

Status: **DRAFT for discussion** (no code changed yet)
Date: 2026-10-08
Scope: `nanosb` TUI command surface (`src/tui/**`) + the project↔agent change-management path (`sandbox/crates/sandbox/src/project.rs`, `src/deploy.rs`, `src/supervisor/**`).

---

## 0. Evidence base (verified)

Three code audits + an Oracle architecture review produced these findings. All file:line references are verified against the current tree.

**Architecture (how changes flow today):**
- `ProjectMount::detect()` (`sandbox/…/project.rs:702`) classifies a project as SingleRepo / MultiRepo / NoGit.
- **NoGit ⇒ `git_init_project()` (`project.rs:423`) runs `git init` on the USER'S SOURCE DIR and commits everything**, then normal clone logic runs. No consent.
- Sandbox gets `git clone --local` at `~/.nanosandbox/clones/<hash>/<id>` (hardlinks), branch `nanosb/<short-id>`, mounted **RW** at `/workspace` via virtiofs (`deploy/mount_planner.rs:127`).
- Changes return to host **only via git**: `auto_commit_and_sync()` (`project.rs:541`) commits clone changes on suspend/teardown and `git fetch`es `refs/heads/<branch>:refs/heads/<branch>` into the source repo.
- Auto-sync (settings default **OFF**): `sync_project_commits()` (`src/tui/app.rs:679`) polls clone HEAD every ~2s and runs `git fetch <clone> <branch>:<branch> --force` into source (`app.rs:755`). Controlled by `/gitsync on|off|now` + `/branches`.
- Secrets: env stripped from config/deploy by `extract_boot_env()` (`src/deploy.rs:109`); delivered via `NANOSB_BOOT_ENV`; session stores only key names (`session.rs:69`). Git author hardcoded; all git ops local.

**Security findings (ranked):**
| # | Severity | Finding | Evidence |
|---|----------|---------|----------|
| S1 | **CRITICAL** | Host RCE: host runs `git status`/`diff`/`log` in the agent-writable clone; agent-set `core.fsmonitor` executes on the host. Reproduced. | `app.rs:610,652,695,724` |
| S2 | **CRITICAL** | `.git` symlink redirection: agent replaces `.git` with a symlink; host commits/fetches into an arbitrary repo. Reproduced. | `project.rs:541`, `app.rs:754` |
| S3 | HIGH | `git fetch … --force` clobbers the user's local branch. | `app.rs:755`, `run.rs:2318` |
| S4 | HIGH | NoGit auto-`git init` mutates the user's source dir (running `from /` footgun). | `project.rs:423,830,1014` |
| S5 | HIGH | virtiofs share not confined to the share (documented residual). | `zero-image-…-security.md` R1 |
| S6 | HIGH | `/edit` opens the agent-controlled clone in git-aware tools (same class as S1). | `run.rs:2349` |
| S7 | MEDIUM | `--env-file` path read with no validation (arbitrary host file read). | `run.rs:4073` (add_agent) |
| S8 | MEDIUM | MCP inline token values written to disk + mounted into guest. | `deploy/config_gen.rs`, `deploy.rs:218` |
| S9 | MEDIUM | `/upload`/`/paste-image`: `host_upload_path`/`fs_upload` lack path-traversal protection (latent — callers strip to basename); follows symlinks; size checked after full read. | `upload.rs:79,87,139`; `run.rs:2614` |
| S10 | LOW | `/env` set un-marks runtime keys → key name persisted to session. | `run.rs:4644` |
| S11 | LOW | Supervisor stop fire-and-forget, no error handling. | `run.rs:1287` |
| S12 | LOW | URLs parsed from terminal output passed to `open` unvalidated. | `terminal.rs:753` |

**Non-findings (correct as-is):** secret stripping to disk/argv; session stores key names only; no shell invocation in `/edit` (uses `Command::new`+`.args`); `detect_file_paths` is dead code (not reachable).

---

## 1. Architecture assessment — the core discussion

**Verdict:** the git-clone + per-sandbox-branch + fetch-back model is a good **transport/audit format** but a **broken trust boundary**. It ingests an *agent-writable* clone into the user's source repo, while the RW virtiofs share means the clone is not a boundary at all (S5). S1/S2 are direct consequences. **Keep git as the format; change who may mutate the source, and treat the clone as untrusted.**

### A1 — Source repo is read-only to nanosb (recommended)
- Remove the setup-time branch creation in the source (`project.rs:280` region / `git_clone_local`).
- Never fetch into `refs/heads/*`; fetch into `refs/nanosb/<id>` only.
- Apply to a user branch only via an explicit command, fast-forward-only by default.
- **Trade-off:** changes are one `git fetch`/`apply` away instead of auto-visible; matches "explicit review" and removes S3 entirely.

### A2 — NoGit: never mutate the source (recommended)
- `git init` **inside the clone**, snapshot the source into it, return changes via an explicit apply (patch/copy), not by mutating the source.
- **Alternative:** keep `git init`-the-source but require explicit consent + hard-refuse `/`, `$HOME`, any `$HOME` subdir, any path with a `.git` ancestor.
- **Trade-off:** A2 unifies git/non-git and removes S4; it needs a non-git return path (apply). Consent-mode is cheaper but still mutates.

### A3 — Treat the clone as untrusted in every host op
- All host git via hardened invocation (see S1 fix) + `.git` real-dir validation (S2 fix).
- `/edit` and any tool we launch against the clone gets a sanitized config or a read-only snapshot (S6).

### A4 — virtiofs confinement (platform; multi-tenant gate)
- Out of scope for a code refactor today; document as the gating item for multi-tenant (dedicated APFS volume + per-sandbox UID; already deferred in the security doc).

**Decision needed:** adopt A1+A2+A3 (recommended), or a subset.

---

## 2. Security fixes (exact changes)

### S1 — Neuter host git config vectors *(must-fix)*
- **New** `src/tui/gitcmd.rs`: `pub fn host_git() -> Command` that injects
  `-c core.fsmonitor=false -c core.hooksPath=/dev/null -c core.pager=cat -c core.editor=false -c core.sshCommand=false -c core.gitProxy= -c core.askPass= -c core.alternateRefsCommand= -c core.attributesFile=/dev/null -c diff.external= -c uploadpack.packObjectsHook= --no-optional-locks`.
- Replace `std::process::Command::new("git")` at `app.rs:610,652,695,724,754` and `run.rs:2187,2313` with `gitcmd::host_git()`.
- Alternative (stronger): move these reads to `git2` (does not run fsmonitor/hooks). Heavier; recommend the `-c` hardening first, git2 later.

### S2 — Validate the clone's `.git` is real *(must-fix)*
- **New** `gitcmd::has_real_git_dir(dir) -> bool`: rejects a symlinked `.git`; for a `.git` gitfile, requires the target to canonicalize **inside** the clone.
- Guard every host git op and every ingest path (`project.rs` ingest + `app.rs`/`run.rs` call sites) with this check; refuse + warn (`set_status_message`) instead of proceeding.

### S3 — No clobbering *(must-fix)*
- Fetch into `refs/nanosb/<id>` (namespaced), never `refs/heads/*`; drop `--force` for auto paths.
- `apply` to a user branch: fast-forward only; refuse non-FF and surface the divergence.

### S4 — NoGit must not mutate source *(must-fix, architecture A2)*
- Move `git_init_project` into the clone; snapshot source → clone.
- If consent-mode is chosen instead: add the `/`, `$HOME` guards.

### S6 — `/edit` sanitization *(must-fix)*
- Before launching a tool, copy the clone to a temp **sanitized** tree (strip `.git/config` dangerous keys, `.git/hooks`, `.gitattributes` filter/diff directives), or open only a non-git view.
- Simplest first cut: strip `core.fsmonitor|pager|editor|sshCommand|hooksPath|gitProxy`, `diff.*`, `filter.*`, `alias.*`, `uploadpack.*` from the clone's `.git/config` before `/edit` opens it.

### S7 — `--env-file` validation *(must-fix)*
- In `add_agent` (`run.rs:4073`): canonicalize; require a **regular file** (reject symlink via `symlink_metadata`); cap size; optionally restrict to project dir or `$HOME`.

### S8 — MCP inline secrets *(defense)*
- Detect literal secret-looking values in MCP env; warn, and prefer `$VAR` indirection; document that MCP configs are written to a guest-visible mount.

### S9 — `/upload` jail *(defense)*
- `fs_upload`/`host_upload_path` (`upload.rs`): canonicalize the resolved host path and require `starts_with(mount_root)`; reject symlinks (`symlink_metadata`); **stat size before reading**; keep the destination to `/workspace/.uploads/<basename>`.

### S10–S12 — low-severity hardening
- S10: mark runtime-`/env` keys ephemeral (don't persist names) or document.
- S11: log/handle supervisor-stop failures.
- S12: validate scheme/host for URLs before `open`.

---

## 3. Command-surface refactor

**All 27 commands work** (no stubs). Changes are behavioral tightening + additions.

### 3.1 Keep, tighten
| Command | Change |
|---|---|
| `/upload`, `/paste-image` | Apply S9 jail; size-before-read; symlink reject |
| `/edit` | Apply S6 sanitization |
| `/gitsync` | Rename to `/sync`; `on` requires confirmation; add `--dry-run`; default OFF |
| `/branches` | Keep; show ahead/behind vs base |
| `/env` | Apply S10; keep masking |
| `/reconnect` | Fix no-TTY panic (`run.rs:2068` uses `crossterm::terminal::size()`); fix silent no-op |
| `/zoom` | Add status message when no panels |
| `/kill`, `/destroy`, `/quit` | Document what syncs on exit (in help) |

### 3.2 Remove
- None (all work).
- Housekeeping: drop the redundant `/theme <name>` entries from `ALL_COMMANDS` (`commands.rs:284-285`); advertise `/q` + `/agent` bare.

### 3.3 Add (priority order)
1. `/diff [--stat]` — review agent changes vs `base_commit` (uses hardened git).
2. `/status` — branch, dirty files, ahead/behind, **what would sync**.
3. `/sync --dry-run` + `/apply` — explicit, reviewed, FF-only apply of `nanosb/<id>`.
4. `/discard` — drop agent changes (reset clone).
5. `/mounts` — show the virtiofs surface (observability).
6. `/exec <cmd...>` — run a command in the focused sandbox (surfaces CLI `exec`); only when the sandbox has the exec channel.
7. `/logs [n]` — tail the console log.
8. `/stop [n|name]` — stop a sandbox without removing the panel.

*(Each new command = enum variant + parse arm + HELP_ENTRIES row + handler + parser test + handler test.)*

---

## 4. Tests

- **S1/S2**: `gitcmd` unit tests (hardening args present; real-dir accepts dir/contained gitfile, rejects symlink/escaping gitfile); an integration test that a `.git` symlink / hostile `core.fsmonitor` is inert.
- **S3**: fetch lands in `refs/nanosb/<id>`; FF-only apply refuses divergence.
- **S4**: NoGit leaves the source untouched (no `.git` created in source).
- **S7/S9**: `--env-file` rejects symlink; upload rejects `..` traversal + symlink + oversize.
- **Reconnect**: handler test with no TTY (no panic).
- **New commands**: parser + handler tests (frame/handler suite pattern in `src/tui/tests.rs`).
- Keep the existing 246 nanosb-cli / 186 sandbox tests green; `cargo check` clean.

---

## 5. Docs
- Update `runtime/docs/zero-image-customization-security.md` residual-risk table (S1/S2/S6 now mitigated; S5 still deferred).
- README: document `/sync` semantics + that changes are namespaced and applied explicitly.
- Help text / `HELP_ENTRIES` regenerated automatically.

---

## 6. Phasing & effort

| Phase | Contents | Effort |
|---|---|---|
| **P0 — must-fix security** | S1, S2, S3, S6, S7 (+ S4 if A2 chosen) | M (1–2d) |
| **P1 — defense-in-depth** | S8, S9, S10–S12 + `/upload` jail | S (0.5–1d) |
| **P2 — command tighten** | 3.1 changes, housekeeping removals | S |
| **P3 — new commands** | 3.3 items 1–4 (review/apply), then 5–8 | M–L |
| **P4 — docs/tests sweep** | §4, §5 | S |

---

## 7. Risks
- **R1** Changing fetch to namespaced refs alters the visible workflow → needs a migration note + `/apply`.
- **R2** Sanitizing `/edit` may surprise users who rely on hooks; gate behind a setting.
- **R3** NoGit A2 needs a return-path design (patch vs copy) → spike first.
- **R4** git2 migration (S1 alt) is larger; keep as follow-up.

---

## 8. Decisions needed (discuss)
1. **A1**: make the source repo read-only (namespaced refs + explicit apply)? *recommend yes.*
2. **A2**: NoGit — never mutate source (return via apply) vs opt-in `git init` with hard guards? *recommend never-mutate.*
3. **S1**: `-c` hardening now, git2 later — OK, or git2 now?
4. **S6**: sanitize clone config before `/edit`, or open a read-only snapshot?
5. **New commands**: adopt the 3.3 set (all / subset)? Which first?
6. **Scope**: implement P0 only, P0–P2, or the full P0–P4?
