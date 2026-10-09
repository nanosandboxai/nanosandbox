//! Project mount management via git clone --local.
//!
//! This module detects git repository layouts within a project directory,
//! creates isolated local clones for sandbox sessions, and manages their lifecycle
//! including auto-commit on teardown.
//!
//! We use `git clone --local` instead of `git worktree` because worktrees create
//! a `.git` FILE containing an absolute host path (`gitdir: /host/path/...`) which
//! doesn't resolve inside a VM mounted via VirtioFS. Local clones create a proper
//! `.git` directory that works in any filesystem namespace.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;
use tracing::warn;

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
# OS — macOS\n\
.DS_Store\n\
._*\n\
.AppleDouble\n\
\n\
# OS — Windows\n\
Thumbs.db\n\
Desktop.ini\n\
$RECYCLE.BIN/\n\
NTUSER.DAT*\n\
ntuser.dat*\n\
AppData/\n\
\n\
# Windows user profile dirs (when project path is ~)\n\
Documents/\n\
Downloads/\n\
Pictures/\n\
Videos/\n\
Music/\n\
Favorites/\n\
Links/\n\
Saved Games/\n\
Searches/\n\
Contacts/\n\
3D Objects/\n\
OneDrive/\n\
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

/// How the project directory maps to git repos.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectLayout {
    /// The project directory itself is a git repository.
    SingleRepo {
        /// Path to the repository root.
        repo_path: PathBuf,
        /// The currently checked-out branch.
        current_branch: String,
    },
    /// The project directory contains multiple git repositories.
    MultiRepo {
        /// Git repositories found as immediate children.
        repos: Vec<GitRepo>,
        /// Non-git items (files/dirs without .git) in the project directory.
        loose_items: Vec<PathBuf>,
    },
    /// No git repositories found.
    NoGit,
}

/// A git repository discovered within a project directory.
#[derive(Debug, Clone, PartialEq)]
pub struct GitRepo {
    /// Path relative to the project directory.
    pub relative_path: PathBuf,
    /// Absolute path to the repository.
    pub absolute_path: PathBuf,
    /// The currently checked-out branch.
    pub current_branch: String,
}

/// Strategy for naming the worktree branch.
#[derive(Debug, Clone)]
pub enum BranchStrategy {
    /// Automatically generate a branch name from the sandbox ID.
    Auto,
    /// Use a specific branch name.
    Named(String),
}

/// Manages project directory mounting into sandboxes via git local clones.
pub struct ProjectMount {
    /// The original project source path.
    pub source_path: PathBuf,
    /// Detected git layout of the project.
    pub layout: ProjectLayout,
    /// Base directory for created clones (inside .nanosb/worktrees/).
    pub worktree_base: Option<PathBuf>,
    /// Branches created during setup, as (repo_path, branch_name) pairs.
    pub created_branches: Vec<(PathBuf, String)>,
    /// Deferred branch info: (repo_path, branch_name) — set when setup_deferred() is used.
    /// Consumed by create_source_branch_and_fetch().
    pub deferred_branch: Option<(PathBuf, String)>,
}

// ── Helpers ──────────────────────────────────────────────────────────

/// Get the current branch name for a git repository.
fn git_current_branch(repo_path: &Path) -> Result<String, String> {
    let repo = git2::Repository::open(repo_path).map_err(|e| {
        let msg = format!("git2 open failed: {}", e);
        warn!("git_current_branch: {} (repo={})", msg, repo_path.display());
        msg
    })?;
    let head = repo.head().map_err(|e| {
        let msg = format!("git2 head failed: {}", e);
        warn!("git_current_branch: {} (repo={})", msg, repo_path.display());
        msg
    })?;
    let branch = head
        .shorthand()
        .unwrap_or("HEAD")
        .to_string();
    Ok(branch)
}

/// Create a local clone of a repository on a new branch.
///
/// 1. Clones from HEAD (no branch created in source repo)
/// 2. Creates the branch locally in the clone
///
/// The clone has a real `.git` directory (not a gitdir file), so git works
/// correctly even when mounted into a VM via VirtioFS.
///
/// The source repo is NEVER modified — no branch is created there.
fn git_clone_local(repo_path: &Path, clone_path: &Path, branch_name: &str) -> Result<(), String> {
    // Clone from current HEAD (default branch) — no source branch created.
    let cloned = git2::build::RepoBuilder::new()
        .clone(repo_path.to_string_lossy().as_ref(), clone_path)
        .map_err(|e| {
            let msg = format!("git2 clone failed: {}", e);
            warn!(
                "git_clone_local: {} (repo={}, clone={})",
                msg,
                repo_path.display(),
                clone_path.display()
            );
            msg
        })?;

    // Create and checkout a new branch in the clone.
    let head_commit = cloned
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|e| {
            let msg = format!("git2 head commit failed: {}", e);
            warn!("git_clone_local: {} (clone={})", msg, clone_path.display());
            msg
        })?;
    let branch = cloned
        .branch(branch_name, &head_commit, false)
        .map_err(|e| {
            let msg = format!("git2 branch create failed: {}", e);
            warn!(
                "git_clone_local: {} (clone={}, branch={})",
                msg,
                clone_path.display(),
                branch_name
            );
            msg
        })?;
    let branch_ref = branch.into_reference();
    cloned
        .set_head(branch_ref.name().unwrap_or("refs/heads/main"))
        .map_err(|e| {
            let msg = format!("git2 set_head failed: {}", e);
            warn!("git_clone_local: {} (branch={})", msg, branch_name);
            msg
        })?;
    cloned
        .checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
        .map_err(|e| {
            let msg = format!("git2 checkout failed: {}", e);
            warn!("git_clone_local: {} (branch={})", msg, branch_name);
            msg
        })?;

    ensure_nanosb_state_gitignored(clone_path);
    Ok(())
}

/// Create a local clone from the current HEAD without creating a named branch in source.
///
/// The clone gets its own local branch named `branch_name`, but no branch
/// is created in the source repo. This keeps the source repo untouched
/// until the user explicitly syncs.
fn git_clone_local_from_head(
    repo_path: &Path,
    clone_path: &Path,
    branch_name: &str,
) -> Result<(), String> {
    // Clone from current HEAD (default branch).
    let cloned = git2::build::RepoBuilder::new()
        .clone(repo_path.to_string_lossy().as_ref(), clone_path)
        .map_err(|e| {
            let msg = format!("git2 clone failed: {}", e);
            warn!(
                "git_clone_local_from_head: {} (repo={}, clone={})",
                msg,
                repo_path.display(),
                clone_path.display()
            );
            msg
        })?;

    // Create and checkout a new branch in the clone.
    let head_commit = cloned
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|e| {
            let msg = format!("git2 head commit failed: {}", e);
            warn!("git_clone_local_from_head: {} (clone={})", msg, clone_path.display());
            msg
        })?;
    let branch = cloned
        .branch(branch_name, &head_commit, false)
        .map_err(|e| {
            let msg = format!("git2 branch create failed: {}", e);
            warn!(
                "git_clone_local_from_head: {} (clone={}, branch={})",
                msg,
                clone_path.display(),
                branch_name
            );
            msg
        })?;
    let branch_ref = branch.into_reference();
    cloned
        .set_head(branch_ref.name().unwrap_or("refs/heads/main"))
        .map_err(|e| {
            let msg = format!("git2 set_head failed: {}", e);
            warn!("git_clone_local_from_head: {} (branch={})", msg, branch_name);
            msg
        })?;
    cloned
        .checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
        .map_err(|e| {
            let msg = format!("git2 checkout failed: {}", e);
            warn!("git_clone_local_from_head: {} (branch={})", msg, branch_name);
            msg
        })?;

    ensure_nanosb_state_gitignored(clone_path);
    Ok(())
}

/// Ensure `.nanosb-state` is excluded from git tracking in the clone.
///
/// Uses `.git/info/exclude` (local to the clone, not tracked by git) so that
/// agent session data stored in `/workspace/.nanosb-state/` doesn't appear
/// as uncommitted changes or get committed with user code.
fn ensure_nanosb_state_gitignored(clone_path: &Path) {
    let exclude_file = clone_path.join(".git/info/exclude");
    let content = std::fs::read_to_string(&exclude_file).unwrap_or_default();
    if !content.lines().any(|l| l.trim() == ".nanosb-state") {
        let entry = if content.ends_with('\n') || content.is_empty() {
            ".nanosb-state\n"
        } else {
            "\n.nanosb-state\n"
        };
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&exclude_file)
            .and_then(|mut f| std::io::Write::write_all(&mut f, entry.as_bytes()));
    }
}

/// Snapshot a non-git source directory into a clone directory.
///
/// 1. Creates the clone directory.
/// 2. Copies all files from source into clone (excluding `.git`).
/// 3. Writes `DEFAULT_GITIGNORE` into the clone if the source has no `.gitignore`.
/// 4. `git init` inside the clone.
/// 5. `git add -A && git commit -m "initial snapshot"` with the nanosandbox author.
///
/// The source directory is NEVER modified — no `.git`, no `.gitignore` written.
fn snapshot_source_to_clone(source: &Path, clone: &Path) -> Result<(), String> {
    std::fs::create_dir_all(clone).map_err(|e| {
        let msg = format!("Failed to create clone dir: {}", e);
        warn!("snapshot_source_to_clone: {} (clone={})", msg, clone.display());
        msg
    })?;

    // Copy all files from source to clone, excluding .git.
    copy_dir_excluding_git(source, clone)?;

    // Write .gitignore into the clone if the source doesn't have one.
    // (We check the source so we don't overwrite a user-provided .gitignore.)
    let source_gitignore = source.join(".gitignore");
    let clone_gitignore = clone.join(".gitignore");
    if !source_gitignore.exists() && !clone_gitignore.exists() {
        std::fs::write(&clone_gitignore, DEFAULT_GITIGNORE).map_err(|e| {
            let msg = format!("Failed to write .gitignore in clone: {}", e);
            warn!("snapshot_source_to_clone: {} (clone={})", msg, clone.display());
            msg
        })?;
    }

    // git init inside the clone.
    let repo = git2::Repository::init(clone).map_err(|e| {
        let msg = format!("git2 init in clone failed: {}", e);
        warn!("snapshot_source_to_clone: {} (clone={})", msg, clone.display());
        msg
    })?;

    // Stage all files (respecting .gitignore).
    let mut index = repo.index().map_err(|e| {
        let msg = format!("git2 index failed: {}", e);
        warn!("snapshot_source_to_clone: {} (clone={})", msg, clone.display());
        msg
    })?;
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .map_err(|e| {
            let msg = format!("git2 add_all failed: {}", e);
            warn!("snapshot_source_to_clone: {} (clone={})", msg, clone.display());
            msg
        })?;
    index.write().map_err(|e| {
        let msg = format!("git2 index write failed: {}", e);
        warn!("snapshot_source_to_clone: {} (clone={})", msg, clone.display());
        msg
    })?;
    let tree_oid = index.write_tree().map_err(|e| {
        let msg = format!("git2 write_tree failed: {}", e);
        warn!("snapshot_source_to_clone: {} (clone={})", msg, clone.display());
        msg
    })?;
    let tree = repo.find_tree(tree_oid).map_err(|e| {
        let msg = format!("git2 find_tree failed: {}", e);
        warn!("snapshot_source_to_clone: {} (clone={})", msg, clone.display());
        msg
    })?;

    // Commit with explicit nanosandbox author.
    let sig = git2::Signature::now("nanosandbox", "nanosb@local").map_err(|e| {
        let msg = format!("git2 signature failed: {}", e);
        warn!("snapshot_source_to_clone: {} (clone={})", msg, clone.display());
        msg
    })?;
    repo.commit(Some("HEAD"), &sig, &sig, "initial snapshot", &tree, &[])
        .map_err(|e| {
            let msg = format!("git2 commit failed: {}", e);
            warn!("snapshot_source_to_clone: {} (clone={})", msg, clone.display());
            msg
        })?;

    ensure_nanosb_state_gitignored(clone);
    Ok(())
}

/// Directories to skip when snapshotting a non-git source into a clone.
/// These are well-known build artifact and dependency directories that are
/// large, irrelevant to sandbox execution, and covered by DEFAULT_GITIGNORE.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".npm",
    "__pycache__",
    ".venv",
    "venv",
    "env",
    ".eggs",
    ".tox",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    "target",
    "build",
    ".gradle",
    ".m2",
    "vendor",
    ".bundle",
    ".dart_tool",
    ".pub-cache",
    ".next",
    ".nuxt",
    ".output",
    ".svelte-kit",
    ".astro",
    "dist",
    ".vite",
    ".cache",
    "coverage",
    ".nyc_output",
    ".terraform",
    ".idea",
    ".vscode",
    "Pods",
    "DerivedData",
    "_build",
    "deps",
    "bin",
    "obj",
    "packages",
    "tmp",
    "temp",
    "out",
    "htmlcov",
    ".vs",
];

/// Recursively copy a directory, skipping `.git` and well-known build artifact dirs.
fn copy_dir_excluding_git(src: &Path, dst: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(src).map_err(|e| {
        let msg = format!("Failed to read source dir: {}", e);
        warn!("copy_dir_excluding_git: {} (src={})", msg, src.display());
        msg
    })? {
        let entry = entry.map_err(|e| {
            let msg = format!("Failed to read dir entry: {}", e);
            warn!("copy_dir_excluding_git: {}", msg);
            msg
        })?;
        let file_type = entry.file_type().map_err(|e| {
            let msg = format!("Failed to get file type: {}", e);
            warn!("copy_dir_excluding_git: {}", msg);
            msg
        })?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip .git entirely.
        if name == ".git" {
            continue;
        }

        // Skip well-known build artifact directories.
        if file_type.is_dir() && SKIP_DIRS.contains(&name_str.as_ref()) {
            continue;
        }

        let src_path = entry.path();
        let dst_path = dst.join(&name);

        if file_type.is_dir() {
            std::fs::create_dir_all(&dst_path).map_err(|e| {
                let msg = format!("Failed to create dir {}: {}", dst_path.display(), e);
                warn!("copy_dir_excluding_git: {}", msg);
                msg
            })?;
            copy_dir_excluding_git(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|e| {
                let msg = format!("Failed to copy {} -> {}: {}", src_path.display(), dst_path.display(), e);
                warn!("copy_dir_excluding_git: {}", msg);
                msg
            })?;
        }
    }
    Ok(())
}

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
///
/// NOTE: This function is only used in tests. Production NoGit paths use
/// `snapshot_source_to_clone()` instead, which never mutates the source.
#[cfg(test)]
fn git_init_project(path: &Path) -> Result<(), String> {
    // Guard: if the directory is already a valid git repo, nothing to do.
    if path.join(".git").exists() {
        // Verify the repo is usable (has HEAD). If not, clean up stale state.
        if git2::Repository::open(path).is_ok() {
            return Ok(());
        }
        // Stale/corrupt .git — remove lock files and let init recreate it.
        let lock = path.join(".git/config.lock");
        if lock.exists() {
            let _ = std::fs::remove_file(&lock);
        }
        let index_lock = path.join(".git/index.lock");
        if index_lock.exists() {
            let _ = std::fs::remove_file(&index_lock);
        }
    }

    // Write generic .gitignore only when one does not already exist.
    let gitignore_path = path.join(".gitignore");
    if !gitignore_path.exists() {
        std::fs::write(&gitignore_path, DEFAULT_GITIGNORE).map_err(|e| {
            let msg = format!("Failed to write .gitignore: {}", e);
            warn!("git_init_project: {} (path={})", msg, path.display());
            msg
        })?;
    }

    // git2::Repository::init is safe to call on an existing repo (idempotent).
    let repo = git2::Repository::init(path).map_err(|e| {
        let msg = format!("git2 init failed: {}", e);
        warn!("git_init_project: {} (path={})", msg, path.display());
        msg
    })?;

    // Stage all files (respecting .gitignore) — equivalent to `git add -A`.
    let mut index = repo.index().map_err(|e| {
        let msg = format!("git2 index failed: {}", e);
        warn!("git_init_project: {} (path={})", msg, path.display());
        msg
    })?;
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .map_err(|e| {
            let msg = format!("git2 add_all failed: {}", e);
            warn!("git_init_project: {} (path={})", msg, path.display());
            msg
        })?;
    index.write().map_err(|e| {
        let msg = format!("git2 index write failed: {}", e);
        warn!("git_init_project: {} (path={})", msg, path.display());
        msg
    })?;
    let tree_oid = index.write_tree().map_err(|e| {
        let msg = format!("git2 write_tree failed: {}", e);
        warn!("git_init_project: {} (path={})", msg, path.display());
        msg
    })?;
    let tree = repo.find_tree(tree_oid).map_err(|e| {
        let msg = format!("git2 find_tree failed: {}", e);
        warn!("git_init_project: {} (path={})", msg, path.display());
        msg
    })?;

    // Commit with explicit author — no global git config required.
    let sig = git2::Signature::now("nanosandbox", "nanosb@local").map_err(|e| {
        let msg = format!("git2 signature failed: {}", e);
        warn!("git_init_project: {} (path={})", msg, path.display());
        msg
    })?;
    repo.commit(Some("HEAD"), &sig, &sig, "initial snapshot", &tree, &[])
        .map_err(|e| {
            let msg = format!("git2 commit failed: {}", e);
            warn!("git_init_project: {} (path={})", msg, path.display());
            msg
        })?;

    Ok(())
}

/// Compute the global clones directory for a given project source path.
///
/// Returns `~/.nanosandbox/clones/<path-hash>/` where `<path-hash>` is a
/// 16-character hex hash of the canonical source path. This keeps clones
/// out of the project directory entirely.
pub fn clones_dir(source_path: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    source_path.to_string_lossy().as_ref().hash(&mut hasher);
    let hash = format!("{:016x}", hasher.finish());

    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".nanosandbox")
        .join("clones")
        .join(hash)
}

/// Resolve a unique branch name by appending `-2`, `-3`, etc. if the desired name already exists.
fn resolve_branch_name(repo_path: &Path, desired: &str) -> String {
    let repo = match git2::Repository::open(repo_path) {
        Ok(r) => r,
        Err(_) => return desired.to_string(),
    };
    if repo.find_branch(desired, git2::BranchType::Local).is_ok() {
        for i in 2..100 {
            let candidate = format!("{}-{}", desired, i);
            if repo.find_branch(&candidate, git2::BranchType::Local).is_err() {
                return candidate;
            }
        }
    }
    desired.to_string()
}

/// Convert a clone branch name to the namespaced ref in the source repo.
///
/// The clone branch `nanosb/abc12345` is fetched to `refs/nanosb/abc12345`
/// in the source, keeping `refs/heads/` untouched.
fn branch_to_nanosb_ref(branch_name: &str) -> String {
    // Strip `refs/heads/` prefix if present, then strip `nanosb/` prefix
    // to get the short-id, then place under `refs/nanosb/`.
    let name = branch_name.trim_start_matches("refs/heads/");
    let short_id = name.trim_start_matches("nanosb/");
    format!("refs/nanosb/{}", short_id)
}

/// Auto-commit any changes in a clone and fetch the branch back to source
/// under a namespaced ref (`refs/nanosb/<short-id>`).
///
/// The clone branch is fetched to `nanosb_ref` in the source repo, keeping
/// the source's `refs/heads/` namespace untouched.
///
/// This does NOT remove the clone directory. Use `auto_commit_fetch_and_remove`
/// if you also want to delete the clone.
fn auto_commit_and_sync(
    source_repo_path: &Path,
    clone_path: &Path,
    branch_name: &str,
    nanosb_ref: &str,
) -> Result<(), String> {
    let clone_repo = git2::Repository::open(clone_path).map_err(|e| {
        let msg = format!("git2 open clone failed: {}", e);
        warn!(
            "auto_commit_and_sync: {} (clone={}, branch={})",
            msg,
            clone_path.display(),
            branch_name
        );
        msg
    })?;

    // Check for uncommitted changes via git2 status.
    let statuses = clone_repo
        .statuses(Some(
            git2::StatusOptions::new()
                .include_untracked(true)
                .recurse_untracked_dirs(true),
        ))
        .map_err(|e| {
            let msg = format!("git2 status failed: {}", e);
            warn!(
                "auto_commit_and_sync: {} (clone={}, branch={})",
                msg,
                clone_path.display(),
                branch_name
            );
            msg
        })?;

    if !statuses.is_empty() {
        // Stage all changes.
        let mut index = clone_repo.index().map_err(|e| {
            let msg = format!("git2 index failed: {}", e);
            warn!("auto_commit_and_sync: {} (clone={})", msg, clone_path.display());
            msg
        })?;
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT | git2::IndexAddOption::CHECK_PATHSPEC, None)
            .map_err(|e| {
                let msg = format!("git2 add_all failed: {}", e);
                warn!("auto_commit_and_sync: {} (clone={})", msg, clone_path.display());
                msg
            })?;
        // Also remove deleted files from index.
        let cb_clone_path = clone_path.to_path_buf();
        index
            .update_all(["*"], Some(&mut |path: &Path, _| {
                // Return 0 to accept the update (remove from index if file deleted).
                let full = cb_clone_path.join(path);
                if full.exists() { 1 } else { 0 }  // 0 = accept removal, 1 = skip
            }))
            .map_err(|e| {
                let msg = format!("git2 update_all failed: {}", e);
                warn!("auto_commit_and_sync: {} (clone={})", msg, clone_path.display());
                msg
            })?;
        index.write().map_err(|e| {
            let msg = format!("git2 index write failed: {}", e);
            warn!("auto_commit_and_sync: {} (clone={})", msg, clone_path.display());
            msg
        })?;
        let tree_oid = index.write_tree().map_err(|e| {
            let msg = format!("git2 write_tree failed: {}", e);
            warn!("auto_commit_and_sync: {} (clone={})", msg, clone_path.display());
            msg
        })?;
        let tree = clone_repo.find_tree(tree_oid).map_err(|e| {
            let msg = format!("git2 find_tree failed: {}", e);
            warn!("auto_commit_and_sync: {} (clone={})", msg, clone_path.display());
            msg
        })?;

        let sig = git2::Signature::now("nanosandbox", "nanosandbox@localhost").map_err(|e| {
            let msg = format!("git2 signature failed: {}", e);
            warn!("auto_commit_and_sync: {} (clone={})", msg, clone_path.display());
            msg
        })?;
        let parent = clone_repo
            .head()
            .and_then(|h| h.peel_to_commit())
            .map_err(|e| {
                let msg = format!("git2 head commit failed: {}", e);
                warn!("auto_commit_and_sync: {} (clone={})", msg, clone_path.display());
                msg
            })?;
        clone_repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "nanosb: auto-save on sandbox destroy",
                &tree,
                &[&parent],
            )
            .map_err(|e| {
                let msg = format!("git2 commit failed: {}", e);
                warn!("auto_commit_and_sync: {} (clone={})", msg, clone_path.display());
                msg
            })?;
    }

    // Fetch the branch from clone back to source repo (update the branch ref).
    let source = git2::Repository::open(source_repo_path).map_err(|e| {
        let msg = format!("git2 open source failed: {}", e);
        warn!(
            "auto_commit_and_sync: {} (source={}, clone={})",
            msg,
            source_repo_path.display(),
            clone_path.display()
        );
        msg
    })?;
    let clone_url = clone_path.to_string_lossy();
    let mut remote = source
        .remote_anonymous(&clone_url)
        .map_err(|e| {
            let msg = format!("git2 remote_anonymous failed: {}", e);
            warn!("auto_commit_and_sync: {} (source={})", msg, source_repo_path.display());
            msg
        })?;
    // Fetch clone branch to namespaced ref in source (no force needed —
    // namespaced refs don't clobber user branches).
    let refspec = format!("+refs/heads/{}:{}", branch_name, nanosb_ref);
    remote
        .fetch(&[&refspec], None, None)
        .map_err(|e| {
            let msg = format!("git2 fetch from clone failed: {}", e);
            warn!(
                "auto_commit_and_sync: {} (source={}, clone={}, branch={}, ref={})",
                msg,
                source_repo_path.display(),
                clone_path.display(),
                branch_name,
                nanosb_ref
            );
            msg
        })?;

    Ok(())
}

/// Auto-commit, fetch branch to source under namespaced ref, and remove the clone directory.
fn auto_commit_and_fetch(
    source_repo_path: &Path,
    clone_path: &Path,
    branch_name: &str,
    nanosb_ref: &str,
) -> Result<(), String> {
    auto_commit_and_sync(source_repo_path, clone_path, branch_name, nanosb_ref)?;
    let _ = std::fs::remove_dir_all(clone_path);
    Ok(())
}

// ── ProjectMount implementation ──────────────────────────────────────

impl ProjectMount {
    /// Detect the git layout of a project directory.
    ///
    /// Returns a `ProjectMount` describing whether the path is a single git repo,
    /// contains multiple git repos, or has no git presence.
    pub fn detect(path: &Path) -> Result<Self, String> {
        let canonical = path.canonicalize().map_err(|e| {
            let msg = format!(
                "Path does not exist or cannot be resolved: {}: {}",
                path.display(),
                e
            );
            warn!("ProjectMount::detect: {}", msg);
            msg
        })?;
        // Reject paths that are the user's home directory — scanning/cloning
        // a home dir is almost never intended.
        if let Some(home) = dirs::home_dir() {
            if let Ok(canon_home) = home.canonicalize() {
                if canonical == canon_home {
                    let msg = format!(
                        "Refusing to use home directory as project path: {}. \
                         Please specify a project subdirectory instead.",
                        canonical.display()
                    );
                    warn!("ProjectMount::detect: {}", msg);
                    return Err(msg);
                }
            }
        }

        // Check if the path itself is a git repo
        if canonical.join(".git").exists() {
            let branch = git_current_branch(&canonical)?;
            return Ok(ProjectMount {
                source_path: canonical.clone(),
                layout: ProjectLayout::SingleRepo {
                    repo_path: canonical,
                    current_branch: branch,
                },
                worktree_base: None,
                created_branches: Vec::new(),
                deferred_branch: None,
            });
        }

        // Scan immediate children for git repos
        let entries = std::fs::read_dir(&canonical).map_err(|e| {
            let msg = format!("Cannot read directory {}: {}", canonical.display(), e);
            warn!("ProjectMount::detect: {}", msg);
            msg
        })?;

        let mut repos = Vec::new();
        let mut loose_items = Vec::new();

        for entry in entries {
            let entry = entry.map_err(|e| {
                let msg = format!("Failed to read directory entry: {}", e);
                warn!(
                    "ProjectMount::detect: {} (path={})",
                    msg,
                    canonical.display()
                );
                msg
            })?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            // Skip hidden directories/files
            if name_str.starts_with('.') {
                continue;
            }

            let child_path = entry.path();

            if child_path.is_dir() && child_path.join(".git").exists() {
                let branch = git_current_branch(&child_path)?;
                repos.push(GitRepo {
                    relative_path: PathBuf::from(&*name_str),
                    absolute_path: child_path,
                    current_branch: branch,
                });
            } else if child_path.is_file() {
                // Only collect loose *files* (Makefile, docker-compose.yml, etc.)
                // — never loose directories.
                loose_items.push(PathBuf::from(&*name_str));
            }
        }

        // Sort for deterministic ordering
        repos.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        loose_items.sort();

        let layout = if repos.is_empty() {
            ProjectLayout::NoGit
        } else {
            ProjectLayout::MultiRepo { repos, loose_items }
        };

        Ok(ProjectMount {
            source_path: canonical,
            layout,
            worktree_base: None,
            created_branches: Vec::new(),
            deferred_branch: None,
        })
    }

    /// Set up local clones for the project, creating an isolated copy for the sandbox.
    ///
    /// Returns the path to the clone base directory that should be mounted into the VM.
    pub fn setup(
        &mut self,
        sandbox_id: &str,
        strategy: &BranchStrategy,
    ) -> Result<PathBuf, String> {
        let short_id = if sandbox_id.len() > 8 {
            &sandbox_id[..8]
        } else {
            sandbox_id
        };

        let branch_name = match strategy {
            BranchStrategy::Auto => format!("nanosb/{}", short_id),
            BranchStrategy::Named(name) => name.clone(),
        };

        match &self.layout {
            ProjectLayout::NoGit => {
                // Create the clone directory first, snapshot the source into it,
                // then treat the clone as the SingleRepo for all subsequent operations.
                // The source directory is NEVER modified.
                let clones = clones_dir(&self.source_path);
                std::fs::create_dir_all(&clones).map_err(|e| {
                    let msg = format!("Failed to create clones dir: {}", e);
                    warn!(
                        "ProjectMount::setup: {} (sandbox_id={}, clones={})",
                        msg, sandbox_id, clones.display()
                    );
                    msg
                })?;
                let clone_path = clones.join(short_id);
                if clone_path.exists() {
                    let _ = std::fs::remove_dir_all(&clone_path);
                }
                // Snapshot source into clone (git init + add + commit inside clone).
                snapshot_source_to_clone(&self.source_path, &clone_path)?;
                // The clone is now a valid git repo with an "initial snapshot" commit.
                // Use the clone as the repo_path for SingleRepo.
                let branch_name = resolve_branch_name(&clone_path, &branch_name);
                // Create the nanosb branch in the clone.
                let clone_repo = git2::Repository::open(&clone_path).map_err(|e| {
                    let msg = format!("git2 open clone failed: {}", e);
                    warn!("ProjectMount::setup: {} (clone={})", msg, clone_path.display());
                    msg
                })?;
                let head_commit = clone_repo.head().and_then(|h| h.peel_to_commit()).map_err(|e| {
                    let msg = format!("git2 head commit failed: {}", e);
                    warn!("ProjectMount::setup: {} (clone={})", msg, clone_path.display());
                    msg
                })?;
                clone_repo.branch(&branch_name, &head_commit, false).map_err(|e| {
                    let msg = format!("git2 branch create failed: {}", e);
                    warn!("ProjectMount::setup: {} (clone={}, branch={})", msg, clone_path.display(), branch_name);
                    msg
                })?;
                let branch_ref: git2::Branch<'_> = clone_repo.find_branch(&branch_name, git2::BranchType::Local)
                    .map_err(|e| {
                        let msg = format!("git2 branch ref failed: {}", e);
                        warn!("ProjectMount::setup: {} (clone={})", msg, clone_path.display());
                        msg
                    })?;
                let branch_ref = branch_ref.into_reference();
                clone_repo.set_head(branch_ref.name().unwrap_or("refs/heads/main")).map_err(|e| {
                    let msg = format!("git2 set_head failed: {}", e);
                    warn!("ProjectMount::setup: {} (clone={})", msg, clone_path.display());
                    msg
                })?;
                clone_repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force())).map_err(|e| {
                    let msg = format!("git2 checkout failed: {}", e);
                    warn!("ProjectMount::setup: {} (clone={})", msg, clone_path.display());
                    msg
                })?;
                // Update layout to SingleRepo pointing at the CLONE, not the source.
                self.layout = ProjectLayout::SingleRepo {
                    repo_path: clone_path.clone(),
                    current_branch: branch_name.clone(),
                };
                self.created_branches.push((clone_path.clone(), branch_name));
                self.worktree_base = Some(clone_path.clone());
                Ok(clone_path)
            }

            ProjectLayout::SingleRepo { repo_path, .. } => {
                let repo_path = repo_path.clone();
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

                // Remove stale clone directory from a previous crashed sandbox
                if clone_path.exists() {
                    let _ = std::fs::remove_dir_all(&clone_path);
                }

                let branch_name = resolve_branch_name(&repo_path, &branch_name);
                git_clone_local(&repo_path, &clone_path, &branch_name)?;

                self.created_branches.push((repo_path.clone(), branch_name));
                self.worktree_base = Some(clone_path.clone());

                Ok(clone_path)
            }

            ProjectLayout::MultiRepo { repos, loose_items } => {
                let repos = repos.clone();
                let loose_items = loose_items.clone();

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

                let base_dir = clones.join(short_id);

                // Remove stale base directory from a previous crashed sandbox
                if base_dir.exists() {
                    let _ = std::fs::remove_dir_all(&base_dir);
                }

                std::fs::create_dir_all(&base_dir).map_err(|e| {
                    let msg = format!("Failed to create clone base: {}", e);
                    warn!(
                        "ProjectMount::setup: {} (sandbox_id={}, base={})",
                        msg,
                        sandbox_id,
                        base_dir.display()
                    );
                    msg
                })?;

                // Create local clone for each sub-repo
                for repo in &repos {
                    let clone_path = base_dir.join(&repo.relative_path);
                    let resolved = resolve_branch_name(&repo.absolute_path, &branch_name);
                    git_clone_local(&repo.absolute_path, &clone_path, &resolved)?;
                    self.created_branches
                        .push((repo.absolute_path.clone(), resolved));
                }

                // Copy loose files (detect() now only collects files, not directories)
                for item in &loose_items {
                    let src = self.source_path.join(item);
                    let dst = base_dir.join(item);

                    #[cfg(unix)]
                    {
                        // Prefer symlink; fall back to copy on failure.
                        if std::os::unix::fs::symlink(&src, &dst).is_err() {
                            std::fs::copy(&src, &dst).map_err(|e| {
                                let msg =
                                    format!("Failed to copy {}: {}", item.display(), e);
                                warn!(
                                    "ProjectMount::setup: {} (sandbox_id={})",
                                    msg, sandbox_id
                                );
                                msg
                            })?;
                        }
                    }

                    #[cfg(not(unix))]
                    {
                        std::fs::copy(&src, &dst).map_err(|e| {
                            let msg = format!("Failed to copy {}: {}", item.display(), e);
                            warn!(
                                "ProjectMount::setup: {} (sandbox_id={})",
                                msg, sandbox_id
                            );
                            msg
                        })?;
                    }
                }

                self.worktree_base = Some(base_dir.clone());

                Ok(base_dir)
            }
        }
    }

    /// Set up local clones WITHOUT creating branches in the source repo.
    ///
    /// The clone works in isolation — the source repo is untouched.
    /// Call `create_source_branch_and_fetch()` later to sync.
    pub fn setup_deferred(
        &mut self,
        sandbox_id: &str,
        strategy: &BranchStrategy,
    ) -> Result<PathBuf, String> {
        let short_id = if sandbox_id.len() > 8 {
            &sandbox_id[..8]
        } else {
            sandbox_id
        };

        let branch_name = match strategy {
            BranchStrategy::Auto => format!("nanosb/{}", short_id),
            BranchStrategy::Named(name) => name.clone(),
        };

        match &self.layout {
            ProjectLayout::NoGit => {
                // Snapshot source into clone, then treat clone as SingleRepo.
                // Source is NEVER modified.
                let clones = clones_dir(&self.source_path);
                std::fs::create_dir_all(&clones).map_err(|e| {
                    let msg = format!("Failed to create clones dir: {}", e);
                    warn!(
                        "ProjectMount::setup_deferred: {} (sandbox_id={}, clones={})",
                        msg, sandbox_id, clones.display()
                    );
                    msg
                })?;
                let clone_path = clones.join(short_id);
                if clone_path.exists() {
                    let _ = std::fs::remove_dir_all(&clone_path);
                }
                snapshot_source_to_clone(&self.source_path, &clone_path)?;
                // The clone is now a valid git repo. Use it as the SingleRepo.
                self.layout = ProjectLayout::SingleRepo {
                    repo_path: clone_path.clone(),
                    current_branch: "main".to_string(),
                };
                // Deferred: no branch created in source (there is no source repo).
                // The clone has its own "main" branch with the initial snapshot.
                // We store deferred info pointing at the clone itself.
                self.deferred_branch = Some((clone_path.clone(), branch_name));
                self.worktree_base = Some(clone_path.clone());
                Ok(clone_path)
            }

            ProjectLayout::SingleRepo { repo_path, .. } => {
                let repo_path = repo_path.clone();
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

            ProjectLayout::MultiRepo { repos, loose_items } => {
                let repos = repos.clone();
                let loose_items = loose_items.clone();
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

                let base_dir = clones.join(short_id);
                if base_dir.exists() {
                    let _ = std::fs::remove_dir_all(&base_dir);
                }

                std::fs::create_dir_all(&base_dir).map_err(|e| {
                    let msg = format!("Failed to create clone base: {}", e);
                    warn!(
                        "ProjectMount::setup_deferred: {} (sandbox_id={}, base={})",
                        msg,
                        sandbox_id,
                        base_dir.display()
                    );
                    msg
                })?;

                for repo in &repos {
                    let clone_path = base_dir.join(&repo.relative_path);
                    git_clone_local_from_head(&repo.absolute_path, &clone_path, &branch_name)?;
                }

                // Copy loose files (detect() now only collects files, not directories)
                for item in &loose_items {
                    let src = self.source_path.join(item);
                    let dst = base_dir.join(item);
                    #[cfg(unix)]
                    {
                        // Prefer symlink; fall back to copy on failure.
                        if std::os::unix::fs::symlink(&src, &dst).is_err() {
                            std::fs::copy(&src, &dst).map_err(|e| {
                                let msg =
                                    format!("Failed to copy {}: {}", item.display(), e);
                                warn!(
                                    "ProjectMount::setup_deferred: {} (sandbox_id={})",
                                    msg, sandbox_id
                                );
                                msg
                            })?;
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        std::fs::copy(&src, &dst).map_err(|e| {
                            let msg = format!("Failed to copy {}: {}", item.display(), e);
                            warn!(
                                "ProjectMount::setup_deferred: {} (sandbox_id={})",
                                msg, sandbox_id
                            );
                            msg
                        })?;
                    }
                }

                // Store deferred info — first repo as representative
                self.deferred_branch = Some((repos[0].absolute_path.clone(), branch_name));

                self.worktree_base = Some(base_dir.clone());
                Ok(base_dir)
            }
        }
    }

    /// Create the branch in the source repo and fetch all clone commits to it.
    ///
    /// Called when user runs `/gitsync now` or `/gitsync on` after a deferred setup.
    pub fn create_source_branch_and_fetch(&mut self) -> Result<(), String> {
        let clone_base = match &self.worktree_base {
            Some(base) => base.clone(),
            None => {
                warn!("ProjectMount::create_source_branch_and_fetch: No clone set up");
                return Err("No clone set up".to_string());
            }
        };

        if !self.created_branches.is_empty() {
            // Already created — just do a fetch to namespaced ref
            for (source_path, branch_name) in &self.created_branches {
                if let Ok(source) = git2::Repository::open(source_path) {
                    let clone_url = clone_base.to_string_lossy();
                    if let Ok(mut remote) = source.remote_anonymous(&clone_url) {
                        let nanosb_ref = branch_to_nanosb_ref(branch_name);
                        let refspec = format!("+refs/heads/{}:{}", branch_name, nanosb_ref);
                        let _ = remote.fetch(&[&refspec], None, None);
                    }
                }
            }
            return Ok(());
        }

        match &self.layout {
            ProjectLayout::SingleRepo { repo_path, .. } => {
                let (_, branch_name) = self.deferred_branch.as_ref().ok_or_else(|| {
                    warn!(
                        "ProjectMount::create_source_branch_and_fetch: No deferred branch info (repo={})",
                        repo_path.display()
                    );
                    "No deferred branch info"
                })?;
                let branch_name = resolve_branch_name(repo_path, branch_name);

                // Create branch in source
                let source = git2::Repository::open(repo_path).map_err(|e| {
                    let msg = format!("git2 open failed: {}", e);
                    warn!("create_source_branch_and_fetch: {} (repo={})", msg, repo_path.display());
                    msg
                })?;
                let head_commit = source.head().and_then(|h| h.peel_to_commit()).map_err(|e| {
                    let msg = format!("git2 head commit failed: {}", e);
                    warn!("create_source_branch_and_fetch: {} (repo={})", msg, repo_path.display());
                    msg
                })?;
                source.branch(&branch_name, &head_commit, false).map_err(|e| {
                    let msg = format!("git2 branch create failed: {}", e);
                    warn!(
                        "create_source_branch_and_fetch: {} (repo={}, branch={})",
                        msg, repo_path.display(), branch_name
                    );
                    msg
                })?;

                // Fetch from clone to namespaced ref in source
                let nanosb_ref = branch_to_nanosb_ref(&branch_name);
                let clone_url = clone_base.to_string_lossy();
                let mut remote = source.remote_anonymous(&clone_url).map_err(|e| {
                    let msg = format!("git2 remote_anonymous failed: {}", e);
                    warn!("create_source_branch_and_fetch: {}", msg);
                    msg
                })?;
                let refspec = format!("+refs/heads/{}:{}", branch_name, nanosb_ref);
                remote.fetch(&[&refspec], None, None).map_err(|e| {
                    let msg = format!("git2 fetch failed: {}", e);
                    warn!(
                        "create_source_branch_and_fetch: {} (repo={}, branch={}, ref={})",
                        msg, repo_path.display(), branch_name, nanosb_ref
                    );
                    msg
                })?;

                self.created_branches.push((repo_path.clone(), branch_name));
                self.deferred_branch = None;
            }

            ProjectLayout::MultiRepo { repos, .. } => {
                let (_, base_branch) = self.deferred_branch.as_ref().ok_or_else(|| {
                    warn!(
                        "ProjectMount::create_source_branch_and_fetch: No deferred branch info (multi-repo)"
                    );
                    "No deferred branch info"
                })?;
                let base_branch = base_branch.clone();

                for repo in repos {
                    let branch_name = resolve_branch_name(&repo.absolute_path, &base_branch);
                    let clone_path = clone_base.join(&repo.relative_path);

                    let source = git2::Repository::open(&repo.absolute_path).map_err(|e| {
                        let msg = format!("git2 open failed: {}", e);
                        warn!(
                            "create_source_branch_and_fetch: {} (repo={})",
                            msg, repo.absolute_path.display()
                        );
                        msg
                    })?;
                    let head_commit = source.head().and_then(|h| h.peel_to_commit()).map_err(|e| {
                        let msg = format!("git2 head commit failed: {}", e);
                        warn!("create_source_branch_and_fetch: {} (repo={})", msg, repo.absolute_path.display());
                        msg
                    })?;
                    source.branch(&branch_name, &head_commit, false).map_err(|e| {
                        let msg = format!("git2 branch create failed in {}: {}", repo.relative_path.display(), e);
                        warn!("create_source_branch_and_fetch: {} (branch={})", msg, branch_name);
                        msg
                    })?;

                    let nanosb_ref = branch_to_nanosb_ref(&branch_name);
                    let clone_url = clone_path.to_string_lossy();
                    if let Ok(mut remote) = source.remote_anonymous(&clone_url) {
                        let refspec = format!("+refs/heads/{}:{}", branch_name, nanosb_ref);
                        let _ = remote.fetch(&[&refspec], None, None);
                    }

                    self.created_branches
                        .push((repo.absolute_path.clone(), branch_name));
                }
                self.deferred_branch = None;
            }

            ProjectLayout::NoGit => {}
        }

        Ok(())
    }

    /// Tear down clones, auto-committing any changes and fetching branches back to source.
    pub fn teardown(&mut self) -> Result<(), String> {
        let clone_base = match &self.worktree_base {
            Some(base) => base.clone(),
            None => return Ok(()),
        };

        match &self.layout {
            ProjectLayout::SingleRepo { repo_path, .. } => {
                let branch = self
                    .created_branches
                    .first()
                    .map(|(_, b)| b.clone())
                    .unwrap_or_default();
                if !branch.is_empty() {
                    let nanosb_ref = branch_to_nanosb_ref(&branch);
                    auto_commit_and_fetch(repo_path, &clone_base, &branch, &nanosb_ref)?;
                } else if let Some((deferred_repo, deferred_branch)) = self.deferred_branch.take() {
                    // Deferred setup: auto-commit, create branch via fetch, then remove clone
                    let nanosb_ref = branch_to_nanosb_ref(&deferred_branch);
                    auto_commit_and_fetch(&deferred_repo, &clone_base, &deferred_branch, &nanosb_ref)?;
                } else {
                    let _ = std::fs::remove_dir_all(&clone_base);
                }
            }

            ProjectLayout::MultiRepo { repos, .. } => {
                if !self.created_branches.is_empty() {
                    for (i, repo) in repos.iter().enumerate() {
                        let clone_path = clone_base.join(&repo.relative_path);
                        if clone_path.exists() {
                            let branch = self
                                .created_branches
                                .get(i)
                                .map(|(_, b)| b.clone())
                                .unwrap_or_default();
                            if !branch.is_empty() {
                                let nanosb_ref = branch_to_nanosb_ref(&branch);
                                auto_commit_and_fetch(&repo.absolute_path, &clone_path, &branch, &nanosb_ref)?;
                            }
                        }
                    }
                } else if let Some((_, deferred_branch)) = self.deferred_branch.take() {
                    // Deferred setup: auto-commit and create branch for each sub-repo
                    let nanosb_ref = branch_to_nanosb_ref(&deferred_branch);
                    for repo in repos {
                        let clone_path = clone_base.join(&repo.relative_path);
                        if clone_path.exists() {
                            auto_commit_and_fetch(
                                &repo.absolute_path,
                                &clone_path,
                                &deferred_branch,
                                &nanosb_ref,
                            )?;
                        }
                    }
                }
                // Remove the base directory (may contain symlinks to loose items)
                let _ = std::fs::remove_dir_all(&clone_base);
            }

            ProjectLayout::NoGit => {
                // After setup() or setup_deferred(), NoGit is replaced with SingleRepo.
                // This arm is only reached if teardown() is called without a prior setup().
            }
        }

        self.worktree_base = None;
        self.created_branches.clear();
        self.deferred_branch = None;

        Ok(())
    }

    /// Suspend the project mount: auto-commit changes and sync branches to source,
    /// but **keep the clone directory intact** for later resume.
    ///
    /// Unlike `teardown()`, this does not delete the clone or clear internal state.
    /// The ProjectMount can later be used with `mount_config()` or serialized into
    /// a session file for resume.
    pub fn suspend(&mut self) -> Result<(), String> {
        let clone_base = match &self.worktree_base {
            Some(base) => base.clone(),
            None => return Ok(()),
        };

        match &self.layout {
            ProjectLayout::SingleRepo { repo_path, .. } => {
                let branch = self
                    .created_branches
                    .first()
                    .map(|(_, b)| b.clone())
                    .unwrap_or_default();
                if !branch.is_empty() {
                    let nanosb_ref = branch_to_nanosb_ref(&branch);
                    auto_commit_and_sync(repo_path, &clone_base, &branch, &nanosb_ref)?;
                } else if let Some((deferred_repo, deferred_branch)) = self.deferred_branch.take() {
                    // Deferred setup: auto-commit changes and create branch in source via fetch.
                    // The clone has a local branch; auto_commit_and_sync will commit uncommitted
                    // changes and `git fetch` will create the branch in the source repo.
                    let nanosb_ref = branch_to_nanosb_ref(&deferred_branch);
                    auto_commit_and_sync(&deferred_repo, &clone_base, &deferred_branch, &nanosb_ref)?;
                    self.created_branches.push((deferred_repo, deferred_branch));
                }
            }

            ProjectLayout::MultiRepo { repos, .. } => {
                if !self.created_branches.is_empty() {
                    for (i, repo) in repos.iter().enumerate() {
                        let clone_path = clone_base.join(&repo.relative_path);
                        if clone_path.exists() {
                            let branch = self
                                .created_branches
                                .get(i)
                                .map(|(_, b)| b.clone())
                                .unwrap_or_default();
                            if !branch.is_empty() {
                                let nanosb_ref = branch_to_nanosb_ref(&branch);
                                auto_commit_and_sync(&repo.absolute_path, &clone_path, &branch, &nanosb_ref)?;
                            }
                        }
                    }
                } else if let Some((_, deferred_branch)) = self.deferred_branch.take() {
                    // Deferred setup: auto-commit and create branch for each sub-repo
                    let nanosb_ref = branch_to_nanosb_ref(&deferred_branch);
                    for repo in repos {
                        let clone_path = clone_base.join(&repo.relative_path);
                        if clone_path.exists() {
                            auto_commit_and_sync(
                                &repo.absolute_path,
                                &clone_path,
                                &deferred_branch,
                                &nanosb_ref,
                            )?;
                            self.created_branches
                                .push((repo.absolute_path.clone(), deferred_branch.clone()));
                        }
                    }
                }
            }

            ProjectLayout::NoGit => {
                // Same as teardown() — only reached if suspend() is called without setup().
            }
        }

        // Note: do NOT clear worktree_base or created_branches — they're needed for resume.
        Ok(())
    }

    /// Resume from an existing clone directory (session recovery).
    ///
    /// Reattaches to a clone that was preserved by a previous `suspend()` call.
    /// Validates the clone exists and restores internal state so that `mount_config()`,
    /// `teardown()`, and `suspend()` work correctly.
    pub fn resume(
        &mut self,
        clone_path: &Path,
        branches: Vec<(PathBuf, String)>,
    ) -> Result<(), String> {
        if !clone_path.exists() {
            let msg = format!(
                "Clone directory not found for resume: {}",
                clone_path.display()
            );
            warn!("ProjectMount::resume: {}", msg);
            return Err(msg);
        }

        // Verify it's actually a git repo (or directory with git repos for multi-repo)
        let has_git = clone_path.join(".git").exists()
            || std::fs::read_dir(clone_path)
                .ok()
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .any(|e| e.path().join(".git").exists())
                })
                .unwrap_or(false);

        if !has_git {
            let msg = format!(
                "Clone directory has no git repo: {}",
                clone_path.display()
            );
            warn!("ProjectMount::resume: {}", msg);
            return Err(msg);
        }

        self.worktree_base = Some(clone_path.to_path_buf());
        self.created_branches = branches;

        Ok(())
    }

    /// Get a mount configuration for the clone, if one has been set up.
    ///
    /// Returns a VirtioFs mount pointing the clone to the given container path.
    pub fn mount_config(&self, container_path: &str) -> Option<runtime::Mount> {
        self.worktree_base
            .as_ref()
            .map(|base| runtime::Mount::virtiofs(base.clone(), container_path))
    }
}



#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper: initialize a git repo in the given directory with an initial commit.
    fn git_init(path: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .expect("git init failed");

        // Set default branch to main
        Command::new("git")
            .args(["checkout", "-b", "main"])
            .current_dir(path)
            .output()
            .expect("git checkout -b main failed");

        // Create initial commit so HEAD exists
        std::fs::write(path.join(".gitkeep"), "").expect("write .gitkeep");
        Command::new("git")
            .args(["add", ".gitkeep"])
            .current_dir(path)
            .output()
            .expect("git add failed");

        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(path)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .expect("git commit failed");
    }

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

    // ── Task 1: struct construction tests ────────────────────────────

    #[test]
    fn test_project_layout_single_repo() {
        let layout = ProjectLayout::SingleRepo {
            repo_path: PathBuf::from("/tmp/repo"),
            current_branch: "main".to_string(),
        };
        assert_eq!(
            layout,
            ProjectLayout::SingleRepo {
                repo_path: PathBuf::from("/tmp/repo"),
                current_branch: "main".to_string(),
            }
        );
    }

    #[test]
    fn test_project_layout_multi_repo() {
        let layout = ProjectLayout::MultiRepo {
            repos: vec![GitRepo {
                relative_path: PathBuf::from("frontend"),
                absolute_path: PathBuf::from("/tmp/project/frontend"),
                current_branch: "main".to_string(),
            }],
            loose_items: vec![PathBuf::from("README.md")],
        };
        match &layout {
            ProjectLayout::MultiRepo { repos, loose_items } => {
                assert_eq!(repos.len(), 1);
                assert_eq!(loose_items.len(), 1);
            }
            _ => panic!("Expected MultiRepo"),
        }
    }

    #[test]
    fn test_project_layout_nogit() {
        let layout = ProjectLayout::NoGit;
        assert_eq!(layout, ProjectLayout::NoGit);
    }

    #[test]
    fn test_branch_strategy_auto() {
        let strategy = BranchStrategy::Auto;
        match strategy {
            BranchStrategy::Auto => {}
            _ => panic!("Expected Auto"),
        }
    }

    #[test]
    fn test_branch_strategy_named() {
        let strategy = BranchStrategy::Named("feat/my-feature".to_string());
        match strategy {
            BranchStrategy::Named(name) => assert_eq!(name, "feat/my-feature"),
            _ => panic!("Expected Named"),
        }
    }

    #[test]
    fn test_project_mount_construction() {
        let pm = ProjectMount {
            source_path: PathBuf::from("/tmp/project"),
            layout: ProjectLayout::NoGit,
            worktree_base: None,
            created_branches: Vec::new(),
            deferred_branch: None,
        };
        assert_eq!(pm.source_path, PathBuf::from("/tmp/project"));
        assert!(pm.worktree_base.is_none());
        assert!(pm.created_branches.is_empty());
    }

    // ── Task 2: detect() tests ───────────────────────────────────────

    #[test]
    fn test_detect_single_repo() {
        let tmp = TempDir::new().unwrap();
        git_init(tmp.path());

        let pm = ProjectMount::detect(tmp.path()).unwrap();
        match &pm.layout {
            ProjectLayout::SingleRepo { current_branch, .. } => {
                assert_eq!(current_branch, "main");
            }
            other => panic!("Expected SingleRepo, got {:?}", other),
        }
    }

    #[test]
    fn test_detect_multi_repo() {
        let tmp = TempDir::new().unwrap();

        // Create two sub-repos
        let repo_a = tmp.path().join("repo-a");
        let repo_b = tmp.path().join("repo-b");
        std::fs::create_dir_all(&repo_a).unwrap();
        std::fs::create_dir_all(&repo_b).unwrap();
        git_init(&repo_a);
        git_init(&repo_b);

        // Create a loose file
        std::fs::write(tmp.path().join("README.md"), "hello").unwrap();

        let pm = ProjectMount::detect(tmp.path()).unwrap();
        match &pm.layout {
            ProjectLayout::MultiRepo { repos, loose_items } => {
                assert_eq!(repos.len(), 2);
                assert_eq!(repos[0].relative_path, PathBuf::from("repo-a"));
                assert_eq!(repos[1].relative_path, PathBuf::from("repo-b"));
                assert_eq!(repos[0].current_branch, "main");
                assert!(loose_items.contains(&PathBuf::from("README.md")));
            }
            other => panic!("Expected MultiRepo, got {:?}", other),
        }
    }

    #[test]
    fn test_detect_no_git() {
        let tmp = TempDir::new().unwrap();
        let pm = ProjectMount::detect(tmp.path()).unwrap();
        assert_eq!(pm.layout, ProjectLayout::NoGit);
    }

    #[test]
    fn test_detect_nonexistent_path() {
        let result = ProjectMount::detect(Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(result.is_err());
    }

    // ── Task 3: setup() tests ────────────────────────────────────────

    #[test]
    fn test_setup_single_repo_clone() {
        let tmp = TempDir::new().unwrap();
        git_init(tmp.path());

        // Create a file and commit it
        std::fs::write(tmp.path().join("hello.txt"), "world").unwrap();
        Command::new("git")
            .args(["add", "hello.txt"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add hello"])
            .current_dir(tmp.path())
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .unwrap();

        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        let clone_dir = pm.setup("abc12345def", &BranchStrategy::Auto).unwrap();

        // Clone should exist and contain hello.txt
        assert!(clone_dir.exists());
        assert!(clone_dir.join("hello.txt").exists());
        assert_eq!(
            std::fs::read_to_string(clone_dir.join("hello.txt")).unwrap(),
            "world"
        );

        // Clone should have a real .git DIRECTORY (not a worktree .git file)
        assert!(clone_dir.join(".git").is_dir());

        // Branch should be created
        assert_eq!(pm.created_branches.len(), 1);
        assert_eq!(pm.created_branches[0].1, "nanosb/abc12345");

        // Source repo must NOT have any refs/heads/nanosb/* branch
        let output = Command::new("git")
            .args(["branch", "--list", "nanosb/*"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let branches = String::from_utf8_lossy(&output.stdout);
        assert!(
            !branches.contains("nanosb/"),
            "Source repo must not have nanosb/* branches after setup, got: {}",
            branches
        );

        // Cleanup
        pm.teardown().unwrap();
    }

    #[test]
    fn test_setup_named_branch() {
        let tmp = TempDir::new().unwrap();
        git_init(tmp.path());

        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        let _worktree = pm
            .setup(
                "test-id",
                &BranchStrategy::Named("feat/my-feature".to_string()),
            )
            .unwrap();

        assert_eq!(pm.created_branches[0].1, "feat/my-feature");

        // The branch exists in the CLONE (not source)
        let output = Command::new("git")
            .args(["branch", "--list", "feat/my-feature"])
            .current_dir(&_worktree)
            .output()
            .unwrap();
        let branch_list = String::from_utf8_lossy(&output.stdout);
        assert!(
            branch_list.contains("feat/my-feature"),
            "Branch feat/my-feature not found in clone: {}",
            branch_list
        );

        // Source must NOT have the branch
        let output = Command::new("git")
            .args(["branch", "--list", "feat/my-feature"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let branch_list = String::from_utf8_lossy(&output.stdout);
        assert!(
            !branch_list.contains("feat/my-feature"),
            "Source must not have branch feat/my-feature, got: {}",
            branch_list
        );

        pm.teardown().unwrap();
    }

    #[test]
    fn test_setup_multi_repo_clones() {
        let tmp = TempDir::new().unwrap();

        let repo_a = tmp.path().join("repo-a");
        let repo_b = tmp.path().join("repo-b");
        std::fs::create_dir_all(&repo_a).unwrap();
        std::fs::create_dir_all(&repo_b).unwrap();
        git_init(&repo_a);
        git_init(&repo_b);

        // Create a loose file
        std::fs::write(tmp.path().join("config.toml"), "[project]").unwrap();

        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        let base = pm.setup("multi123", &BranchStrategy::Auto).unwrap();

        // Both clones should exist with real .git directories
        assert!(base.join("repo-a").exists());
        assert!(base.join("repo-b").exists());
        assert!(base.join("repo-a").join(".git").is_dir());
        assert!(base.join("repo-b").join(".git").is_dir());

        // Loose item should be symlinked (or copied)
        assert!(base.join("config.toml").exists());

        pm.teardown().unwrap();
    }

    #[test]
    fn test_setup_nogit_initialises_git_and_succeeds() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("file.txt"), "hello").unwrap();
        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        assert_eq!(pm.layout, ProjectLayout::NoGit);

        // setup() on a NoGit dir should succeed by snapshotting into a clone.
        let result = pm.setup("test-id", &BranchStrategy::Auto);
        assert!(result.is_ok(), "setup() on NoGit dir should succeed, got: {:?}", result);

        // Source dir must NOT have .git — we never mutate the source.
        assert!(!tmp.path().join(".git").exists(), "source must NOT have .git");

        // Clone should have .git
        let clone_path = result.unwrap();
        assert!(clone_path.join(".git").is_dir(), "clone should have .git");

        pm.teardown().unwrap();
    }

    #[test]
    fn test_setup_creates_clones_dir() {
        let tmp = TempDir::new().unwrap();
        git_init(tmp.path());

        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        let clone_dir = pm.setup("dirtest1", &BranchStrategy::Auto).unwrap();

        // Clone should be in ~/.nanosandbox/clones/, not in the project dir
        assert!(!tmp.path().join(".nanosb").exists());
        // Use the canonical source_path (detect() canonicalizes) to compute the expected dir
        let global_clones = clones_dir(&pm.source_path);
        assert!(global_clones.exists());
        // Clone should have a real .git directory
        assert!(clone_dir.join(".git").is_dir());

        pm.teardown().unwrap();
    }

    // ── Task 4: teardown() tests ─────────────────────────────────────

    #[test]
    fn test_teardown_auto_commits() {
        let tmp = TempDir::new().unwrap();
        git_init(tmp.path());

        // Commit an initial file
        std::fs::write(tmp.path().join("existing.txt"), "initial").unwrap();
        Command::new("git")
            .args(["add", "existing.txt"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add existing"])
            .current_dir(tmp.path())
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .unwrap();

        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        let clone_dir = pm.setup("teardown1", &BranchStrategy::Auto).unwrap();

        // Write a new file in the clone
        std::fs::write(clone_dir.join("new-file.txt"), "new content").unwrap();

        pm.teardown().unwrap();

        // Clone directory should be gone
        assert!(!clone_dir.exists());

        // Check the namespaced ref in source has the auto-save commit (fetched back from clone)
        let output = Command::new("git")
            .args(["log", "--oneline", "refs/nanosb/teardown"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&output.stdout);
        assert!(
            log.contains("auto-save"),
            "Expected auto-save commit in refs/nanosb/teardown, got: {}",
            log
        );
        // Also verify the ref exists
        let output = Command::new("git")
            .args(["show-ref", "refs/nanosb/teardown"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "refs/nanosb/teardown should exist in source"
        );
    }

    #[test]
    fn test_teardown_no_changes_no_commit() {
        let tmp = TempDir::new().unwrap();
        git_init(tmp.path());

        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        let clone_dir = pm.setup("nochange1", &BranchStrategy::Auto).unwrap();

        // Don't make any changes
        pm.teardown().unwrap();

        assert!(!clone_dir.exists());

        // Check the namespaced ref does NOT have an auto-save commit
        let output = Command::new("git")
            .args(["log", "--oneline", "refs/nanosb/nochange"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&output.stdout);
        assert!(
            !log.contains("auto-save"),
            "Should not have auto-save commit, got: {}",
            log
        );
    }

    #[test]
    fn test_teardown_multi_repo() {
        let tmp = TempDir::new().unwrap();

        let repo_a = tmp.path().join("repo-a");
        let repo_b = tmp.path().join("repo-b");
        std::fs::create_dir_all(&repo_a).unwrap();
        std::fs::create_dir_all(&repo_b).unwrap();
        git_init(&repo_a);
        git_init(&repo_b);

        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        let base = pm.setup("multitr1", &BranchStrategy::Auto).unwrap();

        // Write changes in both clones
        std::fs::write(base.join("repo-a").join("change-a.txt"), "aaa").unwrap();
        std::fs::write(base.join("repo-b").join("change-b.txt"), "bbb").unwrap();

        pm.teardown().unwrap();

        // Base directory should be gone
        assert!(!base.exists());

        // Both source repos should have auto-save commits (fetched to namespaced refs)
        for repo in &[&repo_a, &repo_b] {
            let output = Command::new("git")
                .args(["log", "--oneline", "refs/nanosb/multitr1"])
                .current_dir(repo)
                .output()
                .unwrap();
            let log = String::from_utf8_lossy(&output.stdout);
            assert!(
                log.contains("auto-save"),
                "Expected auto-save in {}, got: {}",
                repo.display(),
                log
            );
        }
    }

    // ── Task 5: mount_config() tests ─────────────────────────────────

    #[test]
    fn test_mount_config_returns_virtiofs_mount() {
        let tmp = TempDir::new().unwrap();
        git_init(tmp.path());

        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        let clone_dir = pm.setup("mountcf1", &BranchStrategy::Auto).unwrap();

        let mount = pm.mount_config("/workspace");
        assert!(mount.is_some());

        let mount = mount.unwrap();
        assert_eq!(mount.container_path, "/workspace");
        assert!(!mount.readonly);
        assert_eq!(mount.mount_type, runtime::MountType::VirtioFs);
        assert!(mount.host_path.exists());
        // Clone should have a proper .git directory
        assert!(clone_dir.join(".git").is_dir());

        pm.teardown().unwrap();
    }

    #[test]
    fn test_mount_config_none_before_setup() {
        let tmp = TempDir::new().unwrap();
        git_init(tmp.path());

        let pm = ProjectMount::detect(tmp.path()).unwrap();
        assert!(pm.mount_config("/workspace").is_none());
    }

    // ── Task 4 (deferred): setup_deferred / create_source_branch_and_fetch tests ──

    #[test]
    fn test_setup_deferred_branch_no_source_branch() {
        let tmp = TempDir::new().unwrap();
        git_init(tmp.path());

        std::fs::write(tmp.path().join("hello.txt"), "world").unwrap();
        Command::new("git")
            .args(["add", "hello.txt"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add hello"])
            .current_dir(tmp.path())
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .unwrap();

        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        let clone_dir = pm
            .setup_deferred("abc12345def", &BranchStrategy::Auto)
            .unwrap();

        // Clone should exist with hello.txt
        assert!(clone_dir.exists());
        assert!(clone_dir.join("hello.txt").exists());
        assert!(clone_dir.join(".git").is_dir());

        // No branch should exist in source repo (created_branches should be empty)
        assert!(pm.created_branches.is_empty());

        // Branch should NOT exist in source
        let output = Command::new("git")
            .args(["branch", "--list", "nanosb/abc12345"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let branches = String::from_utf8_lossy(&output.stdout);
        assert!(!branches.contains("nanosb/abc12345"));

        // Clean up clone manually since teardown won't fetch (no created_branches)
        let _ = std::fs::remove_dir_all(&clone_dir);
    }

    #[test]
    fn test_create_source_branch_and_fetch() {
        let tmp = TempDir::new().unwrap();
        git_init(tmp.path());

        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        let clone_dir = pm
            .setup_deferred("synctest1", &BranchStrategy::Auto)
            .unwrap();

        // Write a file in the clone and commit it
        std::fs::write(clone_dir.join("agent-work.txt"), "hello from agent").unwrap();
        Command::new("git")
            .args(["add", "agent-work.txt"])
            .current_dir(&clone_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "agent commit"])
            .current_dir(&clone_dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .unwrap();

        // Now create the branch and fetch
        pm.create_source_branch_and_fetch().unwrap();

        // Branch should now exist in source (created at HEAD)
        assert!(!pm.created_branches.is_empty());
        // The clone content is fetched to the namespaced ref
        let short_id = pm.created_branches[0].1.trim_start_matches("refs/heads/").trim_start_matches("nanosb/");
        let nanosb_ref = format!("refs/nanosb/{}", short_id);
        let output = Command::new("git")
            .args(["log", "--oneline", &nanosb_ref])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&output.stdout);
        assert!(
            log.contains("agent commit"),
            "Expected 'agent commit' in refs/nanosb/synctest1, got: {}",
            log
        );

        pm.teardown().unwrap();
    }

    // ── Deferred branch: suspend() and teardown() commit uncommitted files ──

    #[test]
    fn test_suspend_deferred_commits_uncommitted_files() {
        let tmp = TempDir::new().unwrap();
        git_init(tmp.path());

        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        let clone_dir = pm
            .setup_deferred("suspdef1", &BranchStrategy::Auto)
            .unwrap();

        // Write an uncommitted file in the clone (simulates agent work)
        std::fs::write(clone_dir.join("agent-work.txt"), "hello from agent").unwrap();

        // created_branches should be empty (deferred)
        assert!(pm.created_branches.is_empty());
        assert!(pm.deferred_branch.is_some());

        // Suspend should auto-commit and create the branch in source
        pm.suspend().unwrap();

        // Clone should still exist (suspend preserves it)
        assert!(clone_dir.exists());

        // created_branches should now be populated
        assert_eq!(pm.created_branches.len(), 1);
        assert!(pm.created_branches[0].1.contains("nanosb/suspdef1"));

        // deferred_branch should be consumed
        assert!(pm.deferred_branch.is_none());

        // Namespaced ref should exist in source with the auto-committed file
        let short_id = pm.created_branches[0].1.trim_start_matches("refs/heads/").trim_start_matches("nanosb/");
        let nanosb_ref = format!("refs/nanosb/{}", short_id);
        let output = Command::new("git")
            .args(["log", "--oneline", &nanosb_ref])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&output.stdout);
        assert!(
            log.contains("auto-save"),
            "Expected auto-save commit in source at {}, got: {}",
            nanosb_ref,
            log
        );

        // Cleanup
        let _ = std::fs::remove_dir_all(&clone_dir);
    }

    #[test]
    fn test_teardown_deferred_commits_uncommitted_files() {
        let tmp = TempDir::new().unwrap();
        git_init(tmp.path());

        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        let clone_dir = pm
            .setup_deferred("teardef1", &BranchStrategy::Auto)
            .unwrap();

        // Write an uncommitted file in the clone
        std::fs::write(clone_dir.join("new-feature.txt"), "feature code").unwrap();

        // created_branches should be empty (deferred)
        assert!(pm.created_branches.is_empty());

        // Teardown should auto-commit, create branch in source, and remove clone
        pm.teardown().unwrap();

        // Clone should be gone
        assert!(!clone_dir.exists());

        // Namespaced ref should exist in source with the auto-committed file
        let output = Command::new("git")
            .args(["log", "--oneline", "refs/nanosb/teardef1"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&output.stdout);
        assert!(
            log.contains("auto-save"),
            "Expected auto-save commit in source at refs/nanosb/teardef1, got: {}",
            log
        );
    }

    #[test]
    fn test_suspend_then_resume_preserves_uncommitted() {
        let tmp = TempDir::new().unwrap();
        git_init(tmp.path());

        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        let clone_dir = pm
            .setup_deferred("susres01", &BranchStrategy::Auto)
            .unwrap();

        // Write uncommitted work
        std::fs::write(clone_dir.join("wip.txt"), "work in progress").unwrap();

        // Suspend commits and syncs
        pm.suspend().unwrap();
        let branches = pm.created_branches.clone();

        // Simulate resume: create a new ProjectMount and resume
        let mut pm2 = ProjectMount::detect(tmp.path()).unwrap();
        pm2.resume(&clone_dir, branches).unwrap();

        // The file should still be in the clone (suspend preserved it)
        assert!(clone_dir.join("wip.txt").exists());
        assert_eq!(
            std::fs::read_to_string(clone_dir.join("wip.txt")).unwrap(),
            "work in progress"
        );

        // Clean up
        pm2.teardown().unwrap();
    }

    // ── Task 2: git_init_project tests ───────────────────────────────

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

    #[test]
    fn test_git_init_project_all_ignored_files_succeeds() {
        let tmp = TempDir::new().unwrap();
        // Only create files that will be ignored by DEFAULT_GITIGNORE
        std::fs::create_dir(tmp.path().join("node_modules")).unwrap();
        std::fs::write(tmp.path().join("node_modules").join("pkg.js"), "x").unwrap();

        // Should succeed even though nothing is staged
        git_init_project(tmp.path()).unwrap();
        assert!(tmp.path().join(".git").is_dir());
    }

    #[test]
    fn test_git_init_project_idempotent() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("hello.txt"), "hello").unwrap();

        git_init_project(tmp.path()).unwrap();
        // Second call should succeed without error
        git_init_project(tmp.path()).unwrap();
    }

    #[test]
    fn test_setup_no_git_project_initialises_source_repo() {
        let tmp = TempDir::new().unwrap();
        // Plain directory, no git
        std::fs::write(tmp.path().join("main.py"), "print('hello')").unwrap();

        // detect() should see NoGit initially
        let detected = ProjectMount::detect(tmp.path()).unwrap();
        assert_eq!(detected.layout, ProjectLayout::NoGit);

        // setup() should succeed and return a clone path
        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        let clone_path = pm.setup("nogit001", &BranchStrategy::Auto).unwrap();

        // Source dir must NOT have .git — we never mutate the source.
        assert!(!tmp.path().join(".git").exists(), "source must NOT have .git");

        // Clone exists and has .git
        assert!(clone_path.exists());
        assert!(clone_path.join(".git").is_dir());

        // main.py is in the clone
        assert!(clone_path.join("main.py").exists());

        // Branch nanosb/nogit001 exists in the CLONE (not source)
        let output = Command::new("git")
            .args(["branch", "--list", "nanosb/nogit001"])
            .current_dir(&clone_path)
            .output()
            .unwrap();
        let branches = String::from_utf8_lossy(&output.stdout);
        assert!(branches.contains("nanosb/nogit001"), "expected branch in clone, got: {}", branches);

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

        // Source must NOT have .git
        assert!(!tmp.path().join(".git").exists(), "source must NOT have .git");

        pm.teardown().unwrap();
    }

    #[test]
    fn test_setup_nogit_does_not_mutate_source() {
        // Verify that a non-git source directory is NEVER modified by setup().
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("hello.txt"), "world").unwrap();

        // Record source contents before setup.
        let before: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name()))
            .collect();

        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        let clone_path = pm.setup("nosrc01", &BranchStrategy::Auto).unwrap();

        // Source must NOT have .git or .gitignore.
        assert!(!tmp.path().join(".git").exists(), "source must NOT have .git");
        assert!(!tmp.path().join(".gitignore").exists(), "source must NOT have .gitignore");

        // Source contents must be unchanged.
        let after: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name()))
            .collect();
        assert_eq!(before, after, "source directory contents must not change");

        // Clone must have .git and the file.
        assert!(clone_path.join(".git").is_dir(), "clone must have .git");
        assert!(clone_path.join("hello.txt").exists(), "clone must have the file");

        // Clone must have a valid commit.
        let output = Command::new("git")
            .args(["log", "--oneline"])
            .current_dir(&clone_path)
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&output.stdout);
        assert!(log.contains("initial snapshot"), "clone must have initial snapshot commit");

        pm.teardown().unwrap();
    }

    #[test]
    fn test_setup_deferred_no_git_project() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("app.rs"), "fn main() {}").unwrap();

        // detect() sees NoGit
        let detected = ProjectMount::detect(tmp.path()).unwrap();
        assert_eq!(detected.layout, ProjectLayout::NoGit);

        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        let clone_path = pm.setup_deferred("defr0001", &BranchStrategy::Auto).unwrap();

        // Source must NOT have .git — we never mutate the source.
        assert!(!tmp.path().join(".git").exists(), "source must NOT have .git");

        // Clone exists and has .git
        assert!(clone_path.exists());
        assert!(clone_path.join(".git").is_dir());
        assert!(clone_path.join("app.rs").exists());

        // No branch in source (there is no source repo)
        // The clone has its own "main" branch with the initial snapshot.

        // Cleanup
        let _ = std::fs::remove_dir_all(&clone_path);
    }
}
