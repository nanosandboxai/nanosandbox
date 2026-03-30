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
use std::process::Command;

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
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to run git rev-parse: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git rev-parse failed in {}: {}",
            repo_path.display(),
            stderr.trim()
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Create a local clone of a repository on a new branch.
///
/// 1. Creates the branch in the source repo (`git branch <name>`)
/// 2. Clones locally with hardlinks (`git clone --local --branch <name>`)
///
/// The clone has a real `.git` directory (not a gitdir file), so git works
/// correctly even when mounted into a VM via VirtioFS.
fn git_clone_local(repo_path: &Path, clone_path: &Path, branch_name: &str) -> Result<(), String> {
    // Create the branch in the source repo
    let branch_output = Command::new("git")
        .args(["branch", branch_name])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to run git branch: {}", e))?;

    if !branch_output.status.success() {
        let stderr = String::from_utf8_lossy(&branch_output.stderr);
        return Err(format!(
            "git branch failed in {}: {}",
            repo_path.display(),
            stderr.trim()
        ));
    }

    // Clone locally (--no-hardlinks for cross-device compat in containers)
    let output = Command::new("git")
        .args([
            "clone",
            "--local",
            "--no-hardlinks",
            "--branch",
            branch_name,
            &repo_path.to_string_lossy(),
            &clone_path.to_string_lossy(),
        ])
        .output()
        .map_err(|e| format!("Failed to run git clone --local: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Clean up the branch we created since clone failed
        let _ = Command::new("git")
            .args(["branch", "-D", branch_name])
            .current_dir(repo_path)
            .output();
        return Err(format!("git clone --local failed: {}", stderr.trim()));
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
    // Clone from current HEAD (--no-hardlinks for cross-device compat in containers)
    let output = Command::new("git")
        .args([
            "clone",
            "--local",
            "--no-hardlinks",
            &repo_path.to_string_lossy(),
            &clone_path.to_string_lossy(),
        ])
        .output()
        .map_err(|e| format!("Failed to run git clone --local: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git clone --local failed: {}", stderr.trim()));
    }

    // Create and checkout the branch locally in the clone
    let branch_output = Command::new("git")
        .args(["checkout", "-b", branch_name])
        .current_dir(clone_path)
        .output()
        .map_err(|e| format!("Failed to create branch in clone: {}", e))?;

    if !branch_output.status.success() {
        let stderr = String::from_utf8_lossy(&branch_output.stderr);
        return Err(format!(
            "git checkout -b failed in clone: {}",
            stderr.trim()
        ));
    }

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
    let output = Command::new("git")
        .args(["rev-parse", "--verify", &format!("refs/heads/{}", desired)])
        .current_dir(repo_path)
        .output();

    if let Ok(out) = output {
        if out.status.success() {
            // Branch exists, try with suffix
            for i in 2..100 {
                let candidate = format!("{}-{}", desired, i);
                let check = Command::new("git")
                    .args([
                        "rev-parse",
                        "--verify",
                        &format!("refs/heads/{}", candidate),
                    ])
                    .current_dir(repo_path)
                    .output();
                if let Ok(c) = check {
                    if !c.status.success() {
                        return candidate;
                    }
                } else {
                    return candidate;
                }
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
    // Check for uncommitted changes in the clone
    let status_output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(clone_path)
        .output()
        .map_err(|e| format!("Failed to run git status: {}", e))?;

    if !status_output.status.success() {
        let stderr = String::from_utf8_lossy(&status_output.stderr);
        return Err(format!("git status failed: {}", stderr.trim()));
    }

    let status_text = String::from_utf8_lossy(&status_output.stdout);
    if !status_text.trim().is_empty() {
        // Stage all changes
        let add_output = Command::new("git")
            .args(["add", "-A"])
            .current_dir(clone_path)
            .output()
            .map_err(|e| format!("Failed to run git add: {}", e))?;

        if !add_output.status.success() {
            let stderr = String::from_utf8_lossy(&add_output.stderr);
            return Err(format!("git add failed: {}", stderr.trim()));
        }

        // Commit
        let commit_output = Command::new("git")
            .args(["commit", "-m", "nanosb: auto-save on sandbox destroy"])
            .current_dir(clone_path)
            .env("GIT_AUTHOR_NAME", "nanosandbox")
            .env("GIT_AUTHOR_EMAIL", "nanosandbox@localhost")
            .env("GIT_COMMITTER_NAME", "nanosandbox")
            .env("GIT_COMMITTER_EMAIL", "nanosandbox@localhost")
            .output()
            .map_err(|e| format!("Failed to run git commit: {}", e))?;

        if !commit_output.status.success() {
            let stderr = String::from_utf8_lossy(&commit_output.stderr);
            return Err(format!("git commit failed: {}", stderr.trim()));
        }
    }

    // Fetch the branch from clone back to source repo (update the branch ref)
    let refspec = format!("{}:{}", branch_name, branch_name);
    let fetch_output = Command::new("git")
        .args(["fetch", &clone_path.to_string_lossy(), &refspec, "--force"])
        .current_dir(source_repo_path)
        .output()
        .map_err(|e| format!("Failed to run git fetch: {}", e))?;

    if !fetch_output.status.success() {
        let stderr = String::from_utf8_lossy(&fetch_output.stderr);
        return Err(format!("git fetch from clone failed: {}", stderr.trim()));
    }

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
            format!(
                "Path does not exist or cannot be resolved: {}: {}",
                path.display(),
                e
            )
        })?;

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
        let entries = std::fs::read_dir(&canonical)
            .map_err(|e| format!("Cannot read directory {}: {}", canonical.display(), e))?;

        let mut repos = Vec::new();
        let mut loose_items = Vec::new();

        for entry in entries {
            let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
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
                Err("Cannot setup project clone: no git repository found".to_string())
            }

            ProjectLayout::SingleRepo { repo_path, .. } => {
                let repo_path = repo_path.clone();
                let clones = clones_dir(&self.source_path);
                std::fs::create_dir_all(&clones)
                    .map_err(|e| format!("Failed to create clones dir: {}", e))?;

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
                std::fs::create_dir_all(&clones)
                    .map_err(|e| format!("Failed to create clones dir: {}", e))?;

                let base_dir = clones.join(short_id);

                // Remove stale base directory from a previous crashed sandbox
                if base_dir.exists() {
                    let _ = std::fs::remove_dir_all(&base_dir);
                }

                std::fs::create_dir_all(&base_dir)
                    .map_err(|e| format!("Failed to create clone base: {}", e))?;

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
                                    format!("Failed to copy {}: {}", item.display(), e)
                                })?;
                            }
                        }
                    }

                    #[cfg(not(unix))]
                    {
                        if src.is_dir() {
                            copy_dir_recursive(&src, &dst)?;
                        } else {
                            std::fs::copy(&src, &dst)
                                .map_err(|e| format!("Failed to copy {}: {}", item.display(), e))?;
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
                Err("Cannot setup project clone: no git repository found".to_string())
            }

            ProjectLayout::SingleRepo { repo_path, .. } => {
                let repo_path = repo_path.clone();
                let clones = clones_dir(&self.source_path);
                std::fs::create_dir_all(&clones)
                    .map_err(|e| format!("Failed to create clones dir: {}", e))?;

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
                std::fs::create_dir_all(&clones)
                    .map_err(|e| format!("Failed to create clones dir: {}", e))?;

                let base_dir = clones.join(short_id);
                if base_dir.exists() {
                    let _ = std::fs::remove_dir_all(&base_dir);
                }

                std::fs::create_dir_all(&base_dir)
                    .map_err(|e| format!("Failed to create clone base: {}", e))?;

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
                                    format!("Failed to copy {}: {}", item.display(), e)
                                })?;
                            }
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        if src.is_dir() {
                            copy_dir_recursive(&src, &dst)?;
                        } else {
                            std::fs::copy(&src, &dst)
                                .map_err(|e| format!("Failed to copy {}: {}", item.display(), e))?;
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
            None => return Err("No clone set up".to_string()),
        };

        if !self.created_branches.is_empty() {
            // Already created — just do a fetch
            for (source_path, branch_name) in &self.created_branches {
                let refspec = format!("{}:{}", branch_name, branch_name);
                let _ = Command::new("git")
                    .args(["fetch", &clone_base.to_string_lossy(), &refspec, "--force"])
                    .current_dir(source_path)
                    .output();
            }
            return Ok(());
        }

        match &self.layout {
            ProjectLayout::SingleRepo { repo_path, .. } => {
                let (_, branch_name) = self
                    .deferred_branch
                    .as_ref()
                    .ok_or("No deferred branch info")?;
                let branch_name = resolve_branch_name(repo_path, branch_name);

                // Create branch in source
                let output = Command::new("git")
                    .args(["branch", &branch_name])
                    .current_dir(repo_path)
                    .output()
                    .map_err(|e| format!("git branch failed: {}", e))?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(format!("git branch failed: {}", stderr.trim()));
                }

                // Fetch from clone to source
                let refspec = format!("{}:{}", branch_name, branch_name);
                let fetch = Command::new("git")
                    .args(["fetch", &clone_base.to_string_lossy(), &refspec, "--force"])
                    .current_dir(repo_path)
                    .output()
                    .map_err(|e| format!("git fetch failed: {}", e))?;
                if !fetch.status.success() {
                    let stderr = String::from_utf8_lossy(&fetch.stderr);
                    return Err(format!("git fetch failed: {}", stderr.trim()));
                }

                self.created_branches.push((repo_path.clone(), branch_name));
                self.deferred_branch = None;
            }

            ProjectLayout::MultiRepo { repos, .. } => {
                let (_, base_branch) = self
                    .deferred_branch
                    .as_ref()
                    .ok_or("No deferred branch info")?;
                let base_branch = base_branch.clone();

                for repo in repos {
                    let branch_name = resolve_branch_name(&repo.absolute_path, &base_branch);
                    let clone_path = clone_base.join(&repo.relative_path);

                    let output = Command::new("git")
                        .args(["branch", &branch_name])
                        .current_dir(&repo.absolute_path)
                        .output()
                        .map_err(|e| format!("git branch failed: {}", e))?;
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        return Err(format!(
                            "git branch failed in {}: {}",
                            repo.relative_path.display(),
                            stderr.trim()
                        ));
                    }

                    let refspec = format!("{}:{}", branch_name, branch_name);
                    let _ = Command::new("git")
                        .args(["fetch", &clone_path.to_string_lossy(), &refspec, "--force"])
                        .current_dir(&repo.absolute_path)
                        .output();

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
                // Should not happen since setup() would have failed
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

            ProjectLayout::NoGit => {}
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
            return Err(format!(
                "Clone directory not found for resume: {}",
                clone_path.display()
            ));
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
            return Err(format!(
                "Clone directory has no git repo: {}",
                clone_path.display()
            ));
        }

        self.worktree_base = Some(clone_path.to_path_buf());
        self.created_branches = branches;

        Ok(())
    }

    /// Get a mount configuration for the clone, if one has been set up.
    ///
    /// Returns a VirtioFs mount pointing the clone to the given container path.
    pub fn mount_config(&self, container_path: &str) -> Option<crate::config::Mount> {
        self.worktree_base
            .as_ref()
            .map(|base| crate::config::Mount::virtiofs(base.clone(), container_path))
    }
}

/// Recursively copy a directory.
#[allow(dead_code)]
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst)
        .map_err(|e| format!("Failed to create dir {}: {}", dst.display(), e))?;

    let entries = std::fs::read_dir(src)
        .map_err(|e| format!("Failed to read dir {}: {}", src.display(), e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|e| {
                format!(
                    "Failed to copy {} to {}: {}",
                    src_path.display(),
                    dst_path.display(),
                    e
                )
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
    fn test_setup_nogit_returns_error() {
        let tmp = TempDir::new().unwrap();
        let mut pm = ProjectMount::detect(tmp.path()).unwrap();
        assert_eq!(pm.layout, ProjectLayout::NoGit);

        let result = pm.setup("test-id", &BranchStrategy::Auto);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("no git repository"),
            "Error should mention missing git repository"
        );
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
        assert_eq!(mount.mount_type, crate::config::MountType::VirtioFs);
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
}
