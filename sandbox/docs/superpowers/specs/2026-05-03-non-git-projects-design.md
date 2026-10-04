# Non-Git Project Support Design

**Date:** 2026-05-03  
**Issue:** nanosandboxai/cli#11  
**Repos affected:** `sandbox` (primary), `cli` (follow-up)  
**Branch:** `feature/non-git-projects`

---

## Problem

Projects without a git repository cannot be mounted into sandboxes. `ProjectMount::detect()` classifies them as `ProjectLayout::NoGit`, and both `setup()` and `setup_deferred()` immediately return an error. Additionally, the CLI auto-detection in `main.rs` only sets `project_path` when the CWD contains a `.git` directory, so non-git projects are silently ignored unless the user passes `--project` explicitly.

Multi-sandbox scenarios (multiple agents on the same non-git dir) need a merge story: each agent's work must be independently accessible and combinable.

---

## Chosen Approach: `git init` the Source on First Setup

When `ProjectMount::setup()` or `setup_deferred()` encounters `ProjectLayout::NoGit`, instead of returning an error it initialises a git repository in the **source directory**:

1. Write a generic `.gitignore` to the source dir (only if one does not already exist)
2. `git init` the source dir
3. `git add -A && git commit -m "initial snapshot"`

After this, `ProjectMount::detect()` sees `.git` in the source and classifies it as `ProjectLayout::SingleRepo`. All existing clone, branch, teardown, and session-resume logic applies **unchanged**.

### Why this approach

- **Multi-sandbox isolation**: each sandbox gets its own branch `nanosb/<short-id>` in the source repo, identical to git projects. No sync-back conflicts.
- **User visibility**: agent changes are fetched back to the source repo as named branches. The user can `git checkout nanosb/abc12345` to inspect any agent's work, and merge branches as needed.
- **Security**: only git-tracked changes exit the sandbox on teardown. Files matched by `.gitignore` (secrets, build artifacts, dependencies) are never committed and never synced back.
- **Minimal new code**: one new helper function and a two-line change in each `NoGit` arm.

---

## `.gitignore` Template

A single `DEFAULT_GITIGNORE` constant in `project.rs`. Written to the source dir **before** `git add -A` so matched files are never staged. Applied only when `.gitignore` does not already exist.

Covers:

| Category | Entries |
|---|---|
| Node.js | `node_modules/`, `.npm/`, `*.tsbuildinfo`, `bower_components/`, debug logs |
| Python | `__pycache__/`, `*.pyc`, `.venv/`, `venv/`, `env/`, `ENV/`, pytest/mypy/ruff caches, coverage |
| Rust | `target/` |
| Java/Maven/Gradle | `*.class`, `*.jar`, `*.war`, `target/`, `build/`, `.gradle/`, `.m2/` |
| Go | `vendor/`, `bin/` |
| Ruby | `.bundle/` |
| PHP | `vendor/` |
| C/C++ | `*.o`, `*.a`, `*.so`, `*.dylib` |
| .NET/C# | `bin/`, `obj/`, `.vs/`, `*.user`, `packages/` |
| Swift/Xcode | `Pods/`, `DerivedData/`, `*.xcuserstate` |
| Elixir | `_build/`, `deps/`, `*.beam` |
| Dart/Flutter | `.dart_tool/`, `.pub-cache/` |
| Frontend | `.next/`, `.nuxt/`, `.output/`, `.svelte-kit/`, `.astro/`, `dist/`, `.vite/`, `.cache/` |
| Build output | `build/`, `out/`, `tmp/`, `temp/` |
| Test/Coverage | `coverage/`, `.nyc_output/`, `*.lcov`, `test-results/`, `junit.xml` |
| Terraform | `.terraform/`, `*.tfstate`, `*.tfstate.backup`, `*.tfvars` |
| Secrets | `.env`, `.env.local`, `.env.*.local`, `*.pem`, `*.key`, `credentials.json`, `secrets.json`, `secrets.yml` |
| OS | `.DS_Store`, `._*`, `.AppleDouble`, `Thumbs.db`, `Desktop.ini`, `$RECYCLE.BIN/` |
| IDE/Editor | `.idea/`, `.vscode/`, `*.swp`, `*.swo`, `*~`, `*.iml`, `.sublime-*` |

`Cargo.lock` is intentionally **not** ignored (correct for binary crates).

---

## Code Changes

### `sandbox/crates/sandbox/src/project.rs`

**1. Add constant:**
```rust
pub const DEFAULT_GITIGNORE: &str = r#"..."#;
```

**2. Add helper:**
```rust
fn git_init_project(path: &Path) -> Result<(), String> {
    // Write DEFAULT_GITIGNORE if .gitignore absent
    // git init
    // git add -A
    // git -c user.email=nanosb@local -c user.name=nanosandbox commit -m "initial snapshot"
    // (explicit git config flags so commit succeeds even without global user config)
}
```

**3. Replace `NoGit` arm in `setup()`:**
```rust
ProjectLayout::NoGit => {
    git_init_project(&self.source_path)?;
    // Update layout in-place — do NOT recurse into setup() to avoid infinite loop
    let branch = git_current_branch(&self.source_path)?;
    self.layout = ProjectLayout::SingleRepo {
        repo_path: self.source_path.clone(),
        current_branch: branch,
    };
    // Inline the SingleRepo arm logic here
}
```

**4. Same replacement in `setup_deferred()`.**

### `cli/src/main.rs` (separate PR/commit)

Remove the `.git` guard in two places so CWD is always used as `project_path`:

```rust
// Before:
.or_else(|| { let cwd = ...; if cwd.join(".git").exists() { Some(cwd) } else { None } });
// After:
.or_else(|| std::env::current_dir().ok());
```

Same change for the `.env` auto-detection block at line 383.

---

## Session & Validation

No changes required. `Session` is keyed by path hash — works for any layout. `validate()` checks `clone_path` exists and optionally checks git branches — both work once the source has been `git init`-ed.

---

## Multi-Sandbox Scenario

With `git init` in source:
- Sandbox A → branch `nanosb/aabb1122` in source repo
- Sandbox B → branch `nanosb/ccdd3344` in source repo
- User inspects: `git log nanosb/aabb1122`, `git diff nanosb/aabb1122 nanosb/ccdd3344`
- User merges: standard `git merge` or cherry-pick

This is identical to the existing multi-sandbox flow for git projects.

---

## Out of Scope

- Syncing `.gitignore`-excluded files back to source on teardown (they stay in clone)
- Merging agent branches automatically (user responsibility)
- Removing the `.git` dir from source if the user later deletes all sandboxes
