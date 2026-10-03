# Non-Git Project Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow sandboxes to mount directories that have no git repository by auto-initialising one in the source directory on first use.

**Architecture:** When `ProjectMount::setup()` or `setup_deferred()` encounters `ProjectLayout::NoGit`, it calls a new `git_init_project()` helper that writes a generic `.gitignore` (if absent), runs `git init` + initial commit in the source dir, then updates `self.layout` to `SingleRepo` and inlines the existing `SingleRepo` clone logic — reusing all teardown, suspend, resume, and session code unchanged.

**Tech Stack:** Rust, `std::process::Command` (git), `tempfile` (tests already present in the file)

---

## Files

- Modify: `crates/sandbox/src/project.rs` — add `DEFAULT_GITIGNORE` constant, `git_init_project()` helper, replace `NoGit` arms in `setup()` and `setup_deferred()`

---

### Task 1: Add `DEFAULT_GITIGNORE` constant

**Files:**
- Modify: `crates/sandbox/src/project.rs` (after the `use` block, before `pub enum ProjectLayout`)

- [ ] **Step 1: Add the constant**

Insert the following block at line 17 (after the `use tracing::warn;` line), before the `/// How the project directory maps to git repos.` comment:

```rust
/// Generic `.gitignore` written into a non-git source directory before the initial snapshot.
///
/// Applied only when the source directory has no `.gitignore` already.
/// Covers all major language ecosystems to keep build artifacts, dependencies,
/// and secrets out of the initial commit and out of sandbox sync-back.
pub const DEFAULT_GITIGNORE: &str = "\
# Node.js\n\
node_modules/\n\
.npm/\n\
*.tsbuildinfo\n\
bower_components/\n\
npm-debug.log*\n\
yarn-debug.log*\n\
yarn-error.log*\n\
\n\
# Python\n\
__pycache__/\n\
*.pyc\n\
*.pyo\n\
*.pyd\n\
.venv/\n\
venv/\n\
env/\n\
ENV/\n\
.eggs/\n\
*.egg-info/\n\
.tox/\n\
.pytest_cache/\n\
.mypy_cache/\n\
.ruff_cache/\n\
.coverage\n\
htmlcov/\n\
pip-log.txt\n\
\n\
# Rust\n\
target/\n\
\n\
# Java / Maven / Gradle\n\
*.class\n\
*.jar\n\
*.war\n\
*.ear\n\
target/\n\
build/\n\
.gradle/\n\
.m2/\n\
\n\
# Go\n\
vendor/\n\
bin/\n\
\n\
# Ruby\n\
.bundle/\n\
\n\
# PHP\n\
vendor/\n\
\n\
# C / C++\n\
*.o\n\
*.a\n\
*.so\n\
*.dylib\n\
\n\
# .NET / C#\n\
bin/\n\
obj/\n\
.vs/\n\
*.user\n\
packages/\n\
\n\
# Swift / Xcode\n\
Pods/\n\
DerivedData/\n\
*.xcuserstate\n\
\n\
# Elixir\n\
_build/\n\
deps/\n\
*.beam\n\
\n\
# Dart / Flutter\n\
.dart_tool/\n\
.pub-cache/\n\
\n\
# Frontend frameworks\n\
.next/\n\
out/\n\
.nuxt/\n\
.output/\n\
.svelte-kit/\n\
.astro/\n\
dist/\n\
.vite/\n\
.cache/\n\
\n\
# Build output\n\
build/\n\
out/\n\
tmp/\n\
temp/\n\
\n\
# Test / Coverage\n\
coverage/\n\
.nyc_output/\n\
*.lcov\n\
test-results/\n\
junit.xml\n\
\n\
# Terraform\n\
.terraform/\n\
*.tfstate\n\
*.tfstate.backup\n\
*.tfvars\n\
\n\
# Secrets & environment\n\
.env\n\
.env.local\n\
.env.*.local\n\
*.pem\n\
*.key\n\
credentials.json\n\
secrets.json\n\
secrets.yml\n\
\n\
# OS\n\
.DS_Store\n\
._*\n\
.AppleDouble\n\
Thumbs.db\n\
Desktop.ini\n\
$RECYCLE.BIN/\n\
\n\
# IDE / Editor\n\
.idea/\n\
.vscode/\n\
*.swp\n\
*.swo\n\
*~\n\
*.iml\n\
.sublime-project\n\
.sublime-workspace\n\
";
```

- [ ] **Step 2: Write the test**

In `mod tests` (around line 1318), add after the existing `git_init` helper:

```rust
#[test]
fn test_default_gitignore_covers_node_modules() {
    assert!(DEFAULT_GITIGNORE.contains("node_modules/"));
}

#[test]
fn test_default_gitignore_covers_secrets() {
    assert!(DEFAULT_GITIGNORE.contains(".env\n"));
    assert!(DEFAULT_GITIGNORE.contains("*.pem"));
    assert!(DEFAULT_GITIGNORE.contains("*.key"));
}

#[test]
fn test_default_gitignore_covers_build_artifacts() {
    assert!(DEFAULT_GITIGNORE.contains("target/"));
    assert!(DEFAULT_GITIGNORE.contains("__pycache__/"));
    assert!(DEFAULT_GITIGNORE.contains(".gradle/"));
}
```

- [ ] **Step 3: Run tests to verify they pass**

```bash
cd /Users/janvaca/devdone-labs/sandbox
cargo test -p sandbox test_default_gitignore 2>&1 | tail -20
```

Expected: `3 tests passed`

- [ ] **Step 4: Commit**

```bash
git add crates/sandbox/src/project.rs
git commit -m "feat: add DEFAULT_GITIGNORE constant for non-git project snapshots"
```

---

### Task 2: Add `git_init_project()` helper

**Files:**
- Modify: `crates/sandbox/src/project.rs` — add helper function after `ensure_nanosb_state_gitignored()` (around line 290)

- [ ] **Step 1: Write the failing tests**

In `mod tests`, add:

```rust
#[test]
fn test_git_init_project_creates_git_dir() {
    let tmp = TempDir::new().unwrap();
    // Write a file so there's something to commit
    std::fs::write(tmp.path().join("hello.txt"), "hello").unwrap();

    git_init_project(tmp.path()).unwrap();

    assert!(tmp.path().join(".git").is_dir(), ".git should be created");
}

#[test]
fn test_git_init_project_creates_initial_commit() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("hello.txt"), "hello").unwrap();

    git_init_project(tmp.path()).unwrap();

    let output = Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(log.contains("initial snapshot"), "expected 'initial snapshot' commit, got: {}", log);
}

#[test]
fn test_git_init_project_writes_gitignore_when_absent() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("hello.txt"), "hello").unwrap();

    git_init_project(tmp.path()).unwrap();

    let gitignore = tmp.path().join(".gitignore");
    assert!(gitignore.exists(), ".gitignore should be written");
    let content = std::fs::read_to_string(&gitignore).unwrap();
    assert!(content.contains("node_modules/"));
}

#[test]
fn test_git_init_project_does_not_overwrite_existing_gitignore() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "my_custom_rule/\n").unwrap();
    std::fs::write(tmp.path().join("hello.txt"), "hello").unwrap();

    git_init_project(tmp.path()).unwrap();

    let content = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
    assert_eq!(content, "my_custom_rule/\n", "existing .gitignore must not be overwritten");
    assert!(!content.contains("node_modules/"));
}

#[test]
fn test_git_init_project_gitignored_files_not_committed() {
    let tmp = TempDir::new().unwrap();
    // Create node_modules dir — should be excluded by DEFAULT_GITIGNORE
    std::fs::create_dir(tmp.path().join("node_modules")).unwrap();
    std::fs::write(tmp.path().join("node_modules").join("pkg.js"), "module").unwrap();
    std::fs::write(tmp.path().join("index.js"), "console.log('hi')").unwrap();

    git_init_project(tmp.path()).unwrap();

    // Check that node_modules is not tracked
    let output = Command::new("git")
        .args(["ls-files", "node_modules"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let tracked = String::from_utf8_lossy(&output.stdout);
    assert!(tracked.trim().is_empty(), "node_modules should not be tracked");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd /Users/janvaca/devdone-labs/sandbox
cargo test -p sandbox test_git_init_project 2>&1 | tail -20
```

Expected: compile error — `git_init_project` not defined yet.

- [ ] **Step 3: Implement `git_init_project()`**

Insert after `ensure_nanosb_state_gitignored()` (after line 290):

```rust
/// Initialise a git repository in a non-git source directory.
///
/// Steps:
/// 1. Write `DEFAULT_GITIGNORE` to the directory if no `.gitignore` exists.
/// 2. `git init`
/// 3. `git add -A`
/// 4. `git commit -m "initial snapshot"` (with explicit author config so the
///    commit succeeds even when no global user.name/user.email is configured)
///
/// After this call, `ProjectMount::detect()` will classify the directory as
/// `ProjectLayout::SingleRepo` and all normal clone/branch/teardown logic applies.
fn git_init_project(path: &Path) -> Result<(), String> {
    // Write generic .gitignore only when one does not already exist.
    let gitignore_path = path.join(".gitignore");
    if !gitignore_path.exists() {
        std::fs::write(&gitignore_path, DEFAULT_GITIGNORE).map_err(|e| {
            let msg = format!("Failed to write .gitignore: {}", e);
            warn!("git_init_project: {} (path={})", msg, path.display());
            msg
        })?;
    }

    // git init
    let init = Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .map_err(|e| {
            let msg = format!("Failed to run git init: {}", e);
            warn!("git_init_project: {} (path={})", msg, path.display());
            msg
        })?;
    if !init.status.success() {
        let stderr = String::from_utf8_lossy(&init.stderr);
        let msg = format!("git init failed: {}", stderr.trim());
        warn!("git_init_project: {} (path={})", msg, path.display());
        return Err(msg);
    }

    // git add -A
    let add = Command::new("git")
        .args(["add", "-A"])
        .current_dir(path)
        .output()
        .map_err(|e| {
            let msg = format!("Failed to run git add: {}", e);
            warn!("git_init_project: {} (path={})", msg, path.display());
            msg
        })?;
    if !add.status.success() {
        let stderr = String::from_utf8_lossy(&add.stderr);
        let msg = format!("git add failed: {}", stderr.trim());
        warn!("git_init_project: {} (path={})", msg, path.display());
        return Err(msg);
    }

    // git commit — use explicit author/committer so this works without global git config
    let commit = Command::new("git")
        .args([
            "-c", "user.email=nanosb@local",
            "-c", "user.name=nanosandbox",
            "commit",
            "-m", "initial snapshot",
        ])
        .current_dir(path)
        .output()
        .map_err(|e| {
            let msg = format!("Failed to run git commit: {}", e);
            warn!("git_init_project: {} (path={})", msg, path.display());
            msg
        })?;
    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        let msg = format!("git commit failed: {}", stderr.trim());
        warn!("git_init_project: {} (path={})", msg, path.display());
        return Err(msg);
    }

    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd /Users/janvaca/devdone-labs/sandbox
cargo test -p sandbox test_git_init_project 2>&1 | tail -20
```

Expected: `5 tests passed`

- [ ] **Step 5: Commit**

```bash
git add crates/sandbox/src/project.rs
git commit -m "feat: add git_init_project() helper for non-git source directories"
```

---

### Task 3: Replace `NoGit` arm in `setup()`

**Files:**
- Modify: `crates/sandbox/src/project.rs:611-621`

- [ ] **Step 1: Write the failing test**

In `mod tests`, add:

```rust
#[test]
fn test_setup_no_git_project_initialises_source_repo() {
    let tmp = TempDir::new().unwrap();
    // Plain directory, no git
    std::fs::write(tmp.path().join("main.py"), "print('hello')").unwrap();

    // detect() should see NoGit initially
    let detected = ProjectMount::detect(tmp.path()).unwrap();
    assert_eq!(detected.layout, ProjectLayout::NoGit);

    // setup() should succeed, initialise git in source, and return a clone path
    let mut pm = ProjectMount::detect(tmp.path()).unwrap();
    let clone_path = pm.setup("nogit001", &BranchStrategy::Auto).unwrap();

    // Source dir now has .git
    assert!(tmp.path().join(".git").is_dir(), "source should be git-initialised");

    // Clone exists and has .git
    assert!(clone_path.exists());
    assert!(clone_path.join(".git").is_dir());

    // main.py is in the clone
    assert!(clone_path.join("main.py").exists());

    // Branch nanosb/nogit001 exists in source
    let output = Command::new("git")
        .args(["branch", "--list", "nanosb/nogit001"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let branches = String::from_utf8_lossy(&output.stdout);
    assert!(branches.contains("nanosb/nogit001"), "expected branch in source, got: {}", branches);

    pm.teardown().unwrap();
    assert!(!clone_path.exists(), "clone should be removed after teardown");
}

#[test]
fn test_setup_no_git_project_ignores_node_modules() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir(tmp.path().join("node_modules")).unwrap();
    std::fs::write(tmp.path().join("node_modules").join("pkg.js"), "x").unwrap();
    std::fs::write(tmp.path().join("index.js"), "hello").unwrap();

    let mut pm = ProjectMount::detect(tmp.path()).unwrap();
    let clone_path = pm.setup("npmtest1", &BranchStrategy::Auto).unwrap();

    // node_modules should not be in clone (not tracked by git)
    assert!(!clone_path.join("node_modules").exists(), "node_modules must not be cloned");
    assert!(clone_path.join("index.js").exists());

    pm.teardown().unwrap();
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd /Users/janvaca/devdone-labs/sandbox
cargo test -p sandbox test_setup_no_git 2>&1 | tail -20
```

Expected: both tests FAIL (`setup()` returns `Err` for `NoGit`).

- [ ] **Step 3: Replace the `NoGit` arm in `setup()`**

In `setup()`, replace lines 611–621:

```rust
// BEFORE:
ProjectLayout::NoGit => {
    let msg = "Cannot setup project clone: no git repository found".to_string();
    warn!(
        "ProjectMount::setup: {} (sandbox_id={}, source={})",
        msg,
        sandbox_id,
        self.source_path.display()
    );
    Err(msg)
}
```

With:

```rust
// AFTER:
ProjectLayout::NoGit => {
    // Initialise a git repo in the source directory so we can use the
    // standard clone/branch/teardown flow. After git_init_project() the
    // source has a `.git` dir and an initial snapshot commit.
    git_init_project(&self.source_path).map_err(|e| {
        warn!(
            "ProjectMount::setup: git_init_project failed (sandbox_id={}, source={}): {}",
            sandbox_id,
            self.source_path.display(),
            e
        );
        e
    })?;
    let branch = git_current_branch(&self.source_path).map_err(|e| {
        warn!(
            "ProjectMount::setup: git_current_branch failed after init (sandbox_id={}, source={}): {}",
            sandbox_id,
            self.source_path.display(),
            e
        );
        e
    })?;
    // Update layout in-place — do NOT recurse into setup() to avoid infinite loop.
    self.layout = ProjectLayout::SingleRepo {
        repo_path: self.source_path.clone(),
        current_branch: branch,
    };
    // Inline the SingleRepo arm logic.
    let repo_path = self.source_path.clone();
    let clones = clones_dir(&self.source_path);
    std::fs::create_dir_all(&clones).map_err(|e| {
        let msg = format!("Failed to create clones dir: {}", e);
        warn!(
            "ProjectMount::setup: {} (sandbox_id={}, clones={})",
            msg,
            sandbox_id,
            clones.display()
        );
        msg
    })?;
    let clone_path = clones.join(short_id);
    if clone_path.exists() {
        let _ = std::fs::remove_dir_all(&clone_path);
    }
    let branch_name = resolve_branch_name(&repo_path, &branch_name);
    git_clone_local(&repo_path, &clone_path, &branch_name)?;
    self.created_branches.push((repo_path.clone(), branch_name));
    self.worktree_base = Some(clone_path.clone());
    Ok(clone_path)
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd /Users/janvaca/devdone-labs/sandbox
cargo test -p sandbox test_setup_no_git 2>&1 | tail -20
```

Expected: `2 tests passed`

- [ ] **Step 5: Run full test suite to check no regressions**

```bash
cd /Users/janvaca/devdone-labs/sandbox
cargo test -p sandbox 2>&1 | tail -30
```

Expected: all existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/sandbox/src/project.rs
git commit -m "feat: handle NoGit in setup() by initialising git in source directory"
```

---

### Task 4: Replace `NoGit` arm in `setup_deferred()`

**Files:**
- Modify: `crates/sandbox/src/project.rs:766-774`

- [ ] **Step 1: Write the failing test**

In `mod tests`, add:

```rust
#[test]
fn test_setup_deferred_no_git_project() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("app.rs"), "fn main() {}").unwrap();

    // detect() sees NoGit
    let detected = ProjectMount::detect(tmp.path()).unwrap();
    assert_eq!(detected.layout, ProjectLayout::NoGit);

    let mut pm = ProjectMount::detect(tmp.path()).unwrap();
    let clone_path = pm.setup_deferred("defr0001", &BranchStrategy::Auto).unwrap();

    // Source is now git-initialised
    assert!(tmp.path().join(".git").is_dir());

    // Clone exists
    assert!(clone_path.exists());
    assert!(clone_path.join(".git").is_dir());
    assert!(clone_path.join("app.rs").exists());

    // No branch in source yet (deferred)
    let output = Command::new("git")
        .args(["branch", "--list", "nanosb/defr0001"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let branches = String::from_utf8_lossy(&output.stdout);
    assert!(!branches.contains("nanosb/defr0001"), "branch should not exist in source yet for deferred setup");

    // Cleanup
    let _ = std::fs::remove_dir_all(&clone_path);
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd /Users/janvaca/devdone-labs/sandbox
cargo test -p sandbox test_setup_deferred_no_git 2>&1 | tail -20
```

Expected: FAIL (`setup_deferred()` returns `Err` for `NoGit`).

- [ ] **Step 3: Replace the `NoGit` arm in `setup_deferred()`**

In `setup_deferred()`, replace lines 766–774:

```rust
// BEFORE:
ProjectLayout::NoGit => {
    let msg = "Cannot setup project clone: no git repository found".to_string();
    warn!(
        "ProjectMount::setup_deferred: {} (sandbox_id={}, source={})",
        msg,
        sandbox_id,
        self.source_path.display()
    );
    Err(msg)
}
```

With:

```rust
// AFTER:
ProjectLayout::NoGit => {
    // Initialise git in source then proceed as SingleRepo (deferred variant).
    git_init_project(&self.source_path).map_err(|e| {
        warn!(
            "ProjectMount::setup_deferred: git_init_project failed (sandbox_id={}, source={}): {}",
            sandbox_id,
            self.source_path.display(),
            e
        );
        e
    })?;
    let branch = git_current_branch(&self.source_path).map_err(|e| {
        warn!(
            "ProjectMount::setup_deferred: git_current_branch failed after init (sandbox_id={}, source={}): {}",
            sandbox_id,
            self.source_path.display(),
            e
        );
        e
    })?;
    // Update layout in-place — do NOT recurse.
    self.layout = ProjectLayout::SingleRepo {
        repo_path: self.source_path.clone(),
        current_branch: branch,
    };
    // Inline the SingleRepo deferred arm logic.
    let repo_path = self.source_path.clone();
    let clones = clones_dir(&self.source_path);
    std::fs::create_dir_all(&clones).map_err(|e| {
        let msg = format!("Failed to create clones dir: {}", e);
        warn!(
            "ProjectMount::setup_deferred: {} (sandbox_id={}, clones={})",
            msg,
            sandbox_id,
            clones.display()
        );
        msg
    })?;
    let clone_path = clones.join(short_id);
    if clone_path.exists() {
        let _ = std::fs::remove_dir_all(&clone_path);
    }
    git_clone_local_from_head(&repo_path, &clone_path, &branch_name)?;
    self.deferred_branch = Some((repo_path, branch_name));
    self.worktree_base = Some(clone_path.clone());
    Ok(clone_path)
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd /Users/janvaca/devdone-labs/sandbox
cargo test -p sandbox test_setup_deferred_no_git 2>&1 | tail -20
```

Expected: `1 test passed`

- [ ] **Step 5: Run full test suite**

```bash
cd /Users/janvaca/devdone-labs/sandbox
cargo test -p sandbox 2>&1 | tail -30
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/sandbox/src/project.rs
git commit -m "feat: handle NoGit in setup_deferred() by initialising git in source directory"
```

---

### Task 5: Clean up `teardown()` and `suspend()` NoGit comments

**Files:**
- Modify: `crates/sandbox/src/project.rs:1096-1098` and `:1170`

- [ ] **Step 1: Update stale comments**

In `teardown()`, replace:

```rust
ProjectLayout::NoGit => {
    // Should not happen since setup() would have failed
}
```

With:

```rust
ProjectLayout::NoGit => {
    // After setup() or setup_deferred(), NoGit is replaced with SingleRepo.
    // This arm is only reached if teardown() is called without a prior setup().
}
```

In `suspend()`, replace:

```rust
ProjectLayout::NoGit => {}
```

With:

```rust
ProjectLayout::NoGit => {
    // Same as teardown() — only reached if suspend() is called without setup().
}
```

- [ ] **Step 2: Run full test suite one final time**

```bash
cd /Users/janvaca/devdone-labs/sandbox
cargo test -p sandbox 2>&1 | tail -30
```

Expected: all tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/sandbox/src/project.rs
git commit -m "chore: update stale NoGit comments in teardown() and suspend()"
```

---

### Task 6: CLI — remove `.git` guard in `main.rs`

**Files:**
- Modify: `cli/src/main.rs:383-387` and `:449-458`

> Note: do this commit on the `feature/non-git-projects` branch in the **cli** repo, not sandbox.

- [ ] **Step 1: Fix project_path auto-detection (line 449-458)**

In `cli/src/main.rs`, replace:

```rust
let project_path = cli.project
    .map(std::path::PathBuf::from)
    .or_else(|| {
        let cwd = std::env::current_dir().ok()?;
        if cwd.join(".git").exists() {
            Some(cwd)
        } else {
            None
        }
    });
```

With:

```rust
// Always use CWD as project path so non-git directories get session
// persistence and project mounting (git is initialised in source on first use).
let project_path = cli.project
    .map(std::path::PathBuf::from)
    .or_else(|| std::env::current_dir().ok());
```

- [ ] **Step 2: Fix .env auto-detection (line 383-387)**

Replace:

```rust
let auto_env_dir = cli.project.as_ref().map(std::path::PathBuf::from)
    .or_else(|| {
        let cwd = std::env::current_dir().ok()?;
        if cwd.join(".git").exists() { Some(cwd) } else { None }
    });
```

With:

```rust
let auto_env_dir = cli.project.as_ref().map(std::path::PathBuf::from)
    .or_else(|| std::env::current_dir().ok());
```

- [ ] **Step 3: Build to verify no compile errors**

```bash
cd /Users/janvaca/devdone-labs/cli
cargo build 2>&1 | tail -20
```

Expected: compiles cleanly.

- [ ] **Step 4: Commit**

```bash
cd /Users/janvaca/devdone-labs/cli
git add src/main.rs
git commit -m "feat: auto-detect CWD as project path for non-git directories"
```
