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
/// 1. Creates the branch in the source repo (`git branch <name>`)
/// 2. Clones locally with hardlinks (`git clone --local --branch <name>`)
///
/// The clone has a real `.git` directory (not a gitdir file), so git works
/// correctly even when mounted into a VM via VirtioFS.
fn git_clone_local(repo_path: &Path, clone_path: &Path, branch_name: &str) -> Result<(), String> {
    // Open source repo and create the branch at HEAD.
    let source = git2::Repository::open(repo_path).map_err(|e| {
        let msg = format!("git2 open failed: {}", e);
        warn!("git_clone_local: {} (repo={})", msg, repo_path.display());
        msg
    })?;
    let head_commit = source
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(|e| {
            let msg = format!("git2 head commit failed: {}", e);
            warn!("git_clone_local: {} (repo={})", msg, repo_path.display());
            msg
        })?;
    source
        .branch(branch_name, &head_commit, false)
        .map_err(|e| {
            let msg = format!("git2 branch create failed: {}", e);
            warn!(
                "git_clone_local: {} (repo={}, branch={})",
                msg,
                repo_path.display(),
                branch_name
            );
            msg
        })?;

    // Clone locally using git2. RepoBuilder with local clone (no hardlinks
    // for cross-device compat). Checkout the named branch.
    let mut builder = git2::build::RepoBuilder::new();
    builder.branch(branch_name);
    // Use local clone (copies objects, no hardlinks) for cross-device compat.
    let mut fetch_opts = git2::FetchOptions::new();
    fetch_opts.download_tags(git2::AutotagOption::All);
    builder.fetch_options(fetch_opts);

    let clone_result = builder.clone(
        repo_path.to_string_lossy().as_ref(),
        clone_path,
    );
    if let Err(e) = clone_result {
        // Clean up the branch we created since clone failed.
        let _ = source.find_branch(branch_name, git2::BranchType::Local)
            .and_then(|mut b| b.delete());
        let msg = format!("git2 clone failed: {}", e);
        warn!(
            "git_clone_local: {} (repo={}, clone={}, branch={})",
            msg,
            repo_path.display(),
            clone_path.display(),
            branch_name
        );
        return Err(msg);
    }

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

    // Use git2 (libgit2) so this works on Windows without git CLI installed.
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

/// Auto-commit any changes in a clone and fetch the branch back to source.
///
/// This does NOT remove the clone directory. Use `auto_commit_fetch_and_remove`
/// if you also want to delete the clone.
fn auto_commit_and_sync(
    source_repo_path: &Path,
    clone_path: &Path,
    branch_name: &str,
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
    let refspec = format!("refs/heads/{}:refs/heads/{}", branch_name, branch_name);
    remote
        .fetch(&[&refspec], None, None)
        .map_err(|e| {
            let msg = format!("git2 fetch from clone failed: {}", e);
            warn!(
                "auto_commit_and_sync: {} (source={}, clone={}, branch={})",
                msg,
                source_repo_path.display(),
                clone_path.display(),
                branch_name
            );
            msg
        })?;

    Ok(())
}

/// Auto-commit, fetch branch to source, and remove the clone directory.
fn auto_commit_and_fetch(
    source_repo_path: &Path,
    clone_path: &Path,
    branch_name: &str,
) -> Result<(), String> {
    auto_commit_and_sync(source_repo_path, clone_path, branch_name)?;
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
        // On Windows, canonicalize() returns UNC paths like \\?\C:\... which git
        // interprets as network paths with invalid hostnames. Strip the prefix.
        #[cfg(target_os = "windows")]
        let canonical = {
            let s = canonical.to_string_lossy();
            if let Some(stripped) = s.strip_prefix(r"\\?\") {
                PathBuf::from(stripped)
            } else {
                canonical
            }
        };

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
            } else {
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

                // Symlink (or copy) loose items
                for item in &loose_items {
                    let src = self.source_path.join(item);
                    let dst = base_dir.join(item);

                    #[cfg(unix)]
                    {
                        if std::os::unix::fs::symlink(&src, &dst).is_err() {
                            // Fallback to copy
                            if src.is_dir() {
                                copy_dir_recursive(&src, &dst)?;
                            } else {
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
                    }

                    #[cfg(not(unix))]
                    {
                        if src.is_dir() {
                            copy_dir_recursive(&src, &dst)?;
                        } else {
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

                // Symlink loose items (same as setup)
                for item in &loose_items {
                    let src = self.source_path.join(item);
                    let dst = base_dir.join(item);
                    #[cfg(unix)]
                    {
                        if std::os::unix::fs::symlink(&src, &dst).is_err() {
                            if src.is_dir() {
                                copy_dir_recursive(&src, &dst)?;
                            } else {
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
                    }
                    #[cfg(not(unix))]
                    {
                        if src.is_dir() {
                            copy_dir_recursive(&src, &dst)?;
                        } else {
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
            // Already created — just do a fetch
            for (source_path, branch_name) in &self.created_branches {
                if let Ok(source) = git2::Repository::open(source_path) {
                    let clone_url = clone_base.to_string_lossy();
                    if let Ok(mut remote) = source.remote_anonymous(&clone_url) {
                        let refspec = format!("refs/heads/{}:refs/heads/{}", branch_name, branch_name);
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

                // Fetch from clone to source
                let clone_url = clone_base.to_string_lossy();
                let mut remote = source.remote_anonymous(&clone_url).map_err(|e| {
                    let msg = format!("git2 remote_anonymous failed: {}", e);
                    warn!("create_source_branch_and_fetch: {}", msg);
                    msg
                })?;
                let refspec = format!("refs/heads/{}:refs/heads/{}", branch_name, branch_name);
                remote.fetch(&[&refspec], None, None).map_err(|e| {
                    let msg = format!("git2 fetch failed: {}", e);
                    warn!(
                        "create_source_branch_and_fetch: {} (repo={}, branch={})",
                        msg, repo_path.display(), branch_name
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

                    let clone_url = clone_path.to_string_lossy();
                    if let Ok(mut remote) = source.remote_anonymous(&clone_url) {
                        let refspec = format!("refs/heads/{}:refs/heads/{}", branch_name, branch_name);
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
                    auto_commit_and_fetch(repo_path, &clone_base, &branch)?;
                } else if let Some((deferred_repo, deferred_branch)) = self.deferred_branch.take() {
                    // Deferred setup: auto-commit, create branch via fetch, then remove clone
                    auto_commit_and_fetch(&deferred_repo, &clone_base, &deferred_branch)?;
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
                                auto_commit_and_fetch(&repo.absolute_path, &clone_path, &branch)?;
                            }
                        }
                    }
                } else if let Some((_, deferred_branch)) = self.deferred_branch.take() {
                    // Deferred setup: auto-commit and create branch for each sub-repo
                    for repo in repos {
                        let clone_path = clone_base.join(&repo.relative_path);
                        if clone_path.exists() {
                            auto_commit_and_fetch(
                                &repo.absolute_path,
                                &clone_path,
                                &deferred_branch,
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
                    auto_commit_and_sync(repo_path, &clone_base, &branch)?;
                } else if let Some((deferred_repo, deferred_branch)) = self.deferred_branch.take() {
                    // Deferred setup: auto-commit changes and create branch in source via fetch.
                    // The clone has a local branch; auto_commit_and_sync will commit uncommitted
                    // changes and `git fetch` will create the branch in the source repo.
                    auto_commit_and_sync(&deferred_repo, &clone_base, &deferred_branch)?;
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
                                auto_commit_and_sync(&repo.absolute_path, &clone_path, &branch)?;
                            }
                        }
                    }
                } else if let Some((_, deferred_branch)) = self.deferred_branch.take() {
                    // Deferred setup: auto-commit and create branch for each sub-repo
                    for repo in repos {
                        let clone_path = clone_base.join(&repo.relative_path);
                        if clone_path.exists() {
                            auto_commit_and_sync(
                                &repo.absolute_path,
                                &clone_path,
                                &deferred_branch,
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

/// Recursively copy a directory.
#[allow(dead_code)]
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| {
        let msg = format!("Failed to create dir {}: {}", dst.display(), e);
        warn!("copy_dir_recursive: {}", msg);
        msg
    })?;

    let entries = std::fs::read_dir(src).map_err(|e| {
        let msg = format!("Failed to read dir {}: {}", src.display(), e);
        warn!("copy_dir_recursive: {}", msg);
        msg
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| {
            let msg = format!("Failed to read entry: {}", e);
            warn!("copy_dir_recursive: {} (src={})", msg, src.display());
            msg
        })?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|e| {
                let msg = format!(
                    "Failed to copy {} to {}: {}",
                    src_path.display(),
                    dst_path.display(),
                    e
                );
                warn!("copy_dir_recursive: {}", msg);
                msg
            })?;
        }
    }

    Ok(())
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

        // Verify the branch exists
        let output = Command::new("git")
            .args(["branch", "--list", "feat/my-feature"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let branch_list = String::from_utf8_lossy(&output.stdout);
        assert!(
            branch_list.contains("feat/my-feature"),
            "Branch feat/my-feature not found in: {}",
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

        // After the fix, setup() on a NoGit dir should succeed by initialising git.
        let result = pm.setup("test-id", &BranchStrategy::Auto);
        assert!(result.is_ok(), "setup() on NoGit dir should succeed, got: {:?}", result);

        // Source dir should now have .git
        assert!(tmp.path().join(".git").is_dir());

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

        // Check the branch in source has the auto-save commit (fetched back from clone)
        let output = Command::new("git")
            .args(["log", "--oneline", "nanosb/teardown"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&output.stdout);
        assert!(
            log.contains("auto-save"),
            "Expected auto-save commit, got: {}",
            log
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

        // Check the branch does NOT have an auto-save commit
        let output = Command::new("git")
            .args(["log", "--oneline", "nanosb/nochange"])
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

        // Both source repos should have auto-save commits (fetched back from clones)
        for repo in &[&repo_a, &repo_b] {
            let output = Command::new("git")
                .args(["log", "--oneline", "nanosb/multitr1"])
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

        // Branch should now exist in source
        assert!(!pm.created_branches.is_empty());
        let output = Command::new("git")
            .args(["log", "--oneline", &pm.created_branches[0].1])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&output.stdout);
        assert!(log.contains("agent commit"));

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

        // Branch should exist in source with the auto-committed file
        let output = Command::new("git")
            .args(["log", "--oneline", &pm.created_branches[0].1])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&output.stdout);
        assert!(
            log.contains("auto-save"),
            "Expected auto-save commit in source, got: {}",
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

        // Branch should exist in source with the auto-committed file
        let output = Command::new("git")
            .args(["log", "--oneline", "nanosb/teardef1"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&output.stdout);
        assert!(
            log.contains("auto-save"),
            "Expected auto-save commit in source, got: {}",
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
}
