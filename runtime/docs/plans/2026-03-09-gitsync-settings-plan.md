# GitSync Settings & External Tool Integration — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add user settings for git sync safety control, `/gitsync` and `/open` TUI commands, and suspend-and-launch external tool integration.

**Architecture:** New `src/settings.rs` module for persistent TOML config at `~/.nanosandbox/config.toml`. Modify `project.rs` to support lazy branch creation (no source branch when sync is off). Modify `sync_project_commits()` to respect settings. Add suspend-and-launch pattern in `run.rs` for TUI git tools.

**Tech Stack:** Rust, `toml` crate (TOML serde), ratatui 0.29, crossterm

---

### Task 1: Add `toml` crate dependency

**Files:**
- Modify: `Cargo.toml`

**Step 1: Add toml dependency**

In `Cargo.toml`, add after the `serde_json` line (line 36):

```toml
toml = "0.8"
```

**Step 2: Verify it compiles**

Run: `cargo build --features cli`
Expected: compiles with no errors

**Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add toml crate for user settings"
```

---

### Task 2: Create `src/settings.rs` with `UserSettings` struct

**Files:**
- Create: `src/settings.rs`
- Modify: `src/lib.rs:48` (add `pub mod settings;`)

**Step 1: Write tests for UserSettings**

Create `src/settings.rs` with these tests at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_settings() {
        let settings = UserSettings::default();
        assert!(!settings.gitsync.auto_sync);
        assert!(settings.gitsync.notify_on_commit);
        assert_eq!(settings.tools.editor, "auto");
        assert!(settings.tools.custom_command.is_none());
    }

    #[test]
    fn test_load_missing_file_returns_defaults() {
        let settings = UserSettings::load_from(Path::new("/nonexistent/config.toml"));
        assert!(!settings.gitsync.auto_sync);
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        let mut settings = UserSettings::default();
        settings.gitsync.auto_sync = true;
        settings.tools.editor = "gitui".to_string();

        settings.save_to(&path).unwrap();
        let loaded = UserSettings::load_from(&path);
        assert!(loaded.gitsync.auto_sync);
        assert_eq!(loaded.tools.editor, "gitui");
    }

    #[test]
    fn test_config_path() {
        let path = UserSettings::config_path();
        assert!(path.ends_with("config.toml"));
        assert!(path.to_string_lossy().contains(".nanosandbox"));
    }

    #[test]
    fn test_detect_tool_returns_none_for_fake() {
        assert!(!is_tool_available("definitely_not_a_real_tool_xyz123"));
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --features cli settings::tests`
Expected: compilation error (struct not defined yet)

**Step 3: Write the implementation**

```rust
//! User settings for nanosandbox (persistent config at ~/.nanosandbox/config.toml).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Git sync settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitSyncSettings {
    /// Whether commits auto-sync to local branch.
    /// WARNING: auto_sync modifies your local repo branches automatically.
    /// Default: false (safe).
    #[serde(default)]
    pub auto_sync: bool,
    /// Show system message when agent commits (regardless of auto_sync).
    #[serde(default = "default_true")]
    pub notify_on_commit: bool,
}

impl Default for GitSyncSettings {
    fn default() -> Self {
        Self {
            auto_sync: false,
            notify_on_commit: true,
        }
    }
}

/// External tool settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSettings {
    /// Preferred tool for /open command.
    /// "auto" detects first available. Or: "gitui", "lazygit", "tig", "vscode", "cursor", etc.
    #[serde(default = "default_auto")]
    pub editor: String,
    /// Custom command template when editor = "custom". Use {path} as placeholder.
    #[serde(default)]
    pub custom_command: Option<String>,
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            editor: "auto".to_string(),
            custom_command: None,
        }
    }
}

/// Top-level user settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserSettings {
    /// Git sync behavior.
    #[serde(default)]
    pub gitsync: GitSyncSettings,
    /// External tool preferences.
    #[serde(default)]
    pub tools: ToolSettings,
}

fn default_true() -> bool { true }
fn default_auto() -> String { "auto".to_string() }

impl UserSettings {
    /// Path to the config file: ~/.nanosandbox/config.toml
    pub fn config_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".nanosandbox")
            .join("config.toml")
    }

    /// Load settings from the default config path. Returns defaults if file doesn't exist.
    pub fn load() -> Self {
        Self::load_from(&Self::config_path())
    }

    /// Load settings from a specific path. Returns defaults if file doesn't exist or is invalid.
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => toml::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save settings to the default config path.
    pub fn save(&self) -> Result<(), String> {
        self.save_to(&Self::config_path())
    }

    /// Save settings to a specific path.
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config directory: {}", e))?;
        }
        let content = toml::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize settings: {}", e))?;
        std::fs::write(path, content)
            .map_err(|e| format!("Failed to write config: {}", e))
    }
}

/// Check if a tool binary is available on the system PATH.
pub fn is_tool_available(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Known external tools and their launch commands.
pub struct ToolInfo {
    pub name: &'static str,
    pub binary: &'static str,
    /// true = TUI tool (suspend-and-launch), false = GUI tool (fire-and-forget)
    pub is_tui: bool,
}

/// Ordered list of tools for auto-detection.
pub const KNOWN_TOOLS: &[ToolInfo] = &[
    ToolInfo { name: "gitui", binary: "gitui", is_tui: true },
    ToolInfo { name: "lazygit", binary: "lazygit", is_tui: true },
    ToolInfo { name: "tig", binary: "tig", is_tui: true },
    ToolInfo { name: "vscode", binary: "code", is_tui: false },
    ToolInfo { name: "cursor", binary: "cursor", is_tui: false },
    ToolInfo { name: "gitkraken", binary: "gitkraken", is_tui: false },
    ToolInfo { name: "fork", binary: "fork", is_tui: false },
];

/// Resolve which tool to use based on settings or auto-detection.
/// Returns (binary_name, is_tui) or None if no tool found.
pub fn resolve_tool(editor_pref: &str) -> Option<(&'static str, bool)> {
    if editor_pref == "auto" {
        for tool in KNOWN_TOOLS {
            if is_tool_available(tool.binary) {
                return Some((tool.binary, tool.is_tui));
            }
        }
        None
    } else if editor_pref == "custom" {
        None // handled separately via custom_command
    } else {
        KNOWN_TOOLS.iter()
            .find(|t| t.name == editor_pref)
            .filter(|t| is_tool_available(t.binary))
            .map(|t| (t.binary, t.is_tui))
    }
}
```

**Step 4: Add module to lib.rs**

In `src/lib.rs`, add after `pub mod project;` (line 48):

```rust
pub mod settings;
```

**Step 5: Run tests**

Run: `cargo test --features cli settings::tests`
Expected: all 5 tests pass

**Step 6: Commit**

```bash
git add src/settings.rs src/lib.rs
git commit -m "feat: add UserSettings with TOML config load/save and tool detection"
```

---

### Task 3: Add `/gitsync` and `/open` command parsing

**Files:**
- Modify: `src/tui/commands.rs`

**Step 1: Write tests for new commands**

Add to the bottom of the tests module in `src/tui/commands.rs`:

```rust
#[test]
fn test_parse_gitsync_status() {
    assert_eq!(parse_command("/gitsync"), Some(Command::GitSync { action: None }));
}

#[test]
fn test_parse_gitsync_on() {
    assert_eq!(
        parse_command_verbose("/gitsync on"),
        ParseResult::Ok(Command::GitSync { action: Some("on".to_string()) })
    );
}

#[test]
fn test_parse_gitsync_off() {
    assert_eq!(
        parse_command_verbose("/gitsync off"),
        ParseResult::Ok(Command::GitSync { action: Some("off".to_string()) })
    );
}

#[test]
fn test_parse_gitsync_now() {
    assert_eq!(
        parse_command_verbose("/gitsync now"),
        ParseResult::Ok(Command::GitSync { action: Some("now".to_string()) })
    );
}

#[test]
fn test_parse_gitsync_invalid() {
    let result = parse_command_verbose("/gitsync foo");
    assert!(matches!(result, ParseResult::Err(_)));
}

#[test]
fn test_parse_open_default() {
    assert_eq!(parse_command("/open"), Some(Command::Open { tool: None }));
}

#[test]
fn test_parse_open_specific_tool() {
    assert_eq!(
        parse_command("/open gitui"),
        Some(Command::Open { tool: Some("gitui".to_string()) })
    );
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --features cli commands::tests`
Expected: compilation error (Command variants not defined)

**Step 3: Add command variants and parsing**

Add to the `Command` enum (after `Diff` at line 79):

```rust
/// Git sync control: show status, enable, disable, or manual sync.
GitSync {
    /// Subcommand: None (status), "on", "off", "now"
    action: Option<String>,
},
/// Open clone directory in an external tool.
Open {
    /// Tool override, or None for preferred/auto-detected.
    tool: Option<String>,
},
```

Add to `ALL_COMMANDS` array (after `"/diff"`):

```rust
"/gitsync", "/gitsync on", "/gitsync off", "/gitsync now",
"/open",
```

Add match arms in `parse_command_verbose` (after the `/diff` arm at line 143):

```rust
"/gitsync" => parse_gitsync(&parts),
"/open" => {
    let tool = parts.get(1).map(|s| s.to_string());
    ParseResult::Ok(Command::Open { tool })
}
```

Add the `parse_gitsync` function (after `parse_kill`):

```rust
fn parse_gitsync(parts: &[&str]) -> ParseResult {
    match parts.get(1).copied() {
        None => ParseResult::Ok(Command::GitSync { action: None }),
        Some("on") | Some("off") | Some("now") => {
            ParseResult::Ok(Command::GitSync {
                action: Some(parts[1].to_string()),
            })
        }
        Some(other) => ParseResult::Err(format!(
            "Unknown gitsync action: '{}'\n\
             Usage: /gitsync [on|off|now]\n\
             - /gitsync     Show sync status\n\
             - /gitsync on  Enable auto-sync (unsafe: modifies local branches)\n\
             - /gitsync off Disable auto-sync\n\
             - /gitsync now Sync once manually",
            other,
        )),
    }
}
```

**Step 4: Run tests**

Run: `cargo test --features cli commands::tests`
Expected: all tests pass (existing + 7 new)

**Step 5: Commit**

```bash
git add src/tui/commands.rs
git commit -m "feat: add /gitsync and /open command parsing"
```

---

### Task 4: Modify `project.rs` for lazy branch creation

**Files:**
- Modify: `src/project.rs`

**Step 1: Write test for clone-without-source-branch**

Add to tests in `src/project.rs`:

```rust
#[test]
fn test_setup_deferred_branch_no_source_branch() {
    let tmp = TempDir::new().unwrap();
    git_init(tmp.path());

    std::fs::write(tmp.path().join("hello.txt"), "world").unwrap();
    Command::new("git")
        .args(["add", "hello.txt"])
        .current_dir(tmp.path())
        .output().unwrap();
    Command::new("git")
        .args(["commit", "-m", "add hello"])
        .current_dir(tmp.path())
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output().unwrap();

    let mut pm = ProjectMount::detect(tmp.path()).unwrap();
    let clone_dir = pm.setup_deferred("abc12345def", &BranchStrategy::Auto).unwrap();

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
        .output().unwrap();
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
    let clone_dir = pm.setup_deferred("synctest1", &BranchStrategy::Auto).unwrap();

    // Write a file in the clone and commit it
    std::fs::write(clone_dir.join("agent-work.txt"), "hello from agent").unwrap();
    Command::new("git")
        .args(["add", "agent-work.txt"])
        .current_dir(&clone_dir)
        .output().unwrap();
    Command::new("git")
        .args(["commit", "-m", "agent commit"])
        .current_dir(&clone_dir)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output().unwrap();

    // Now create the branch and fetch
    pm.create_source_branch_and_fetch().unwrap();

    // Branch should now exist in source
    assert!(!pm.created_branches.is_empty());
    let output = Command::new("git")
        .args(["log", "--oneline", &pm.created_branches[0].1])
        .current_dir(tmp.path())
        .output().unwrap();
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(log.contains("agent commit"));

    pm.teardown().unwrap();
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --features cli project::tests::test_setup_deferred`
Expected: compilation error (`setup_deferred` not defined)

**Step 3: Implement `setup_deferred()` and `create_source_branch_and_fetch()`**

Add a new helper function `git_clone_local_from_head` (after `git_clone_local`):

```rust
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
    // Clone from current HEAD (no --branch flag, just default branch)
    let output = Command::new("git")
        .args([
            "clone",
            "--local",
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
        return Err(format!("git checkout -b failed in clone: {}", stderr.trim()));
    }

    Ok(())
}
```

Add `setup_deferred` method to `ProjectMount` impl (after `setup`):

```rust
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

            // Store the deferred branch name for later use
            self.deferred_branch = Some((repo_path, branch_name));

            let branch = &self.deferred_branch.as_ref().unwrap().1;
            git_clone_local_from_head(
                &self.deferred_branch.as_ref().unwrap().0,
                &clone_path,
                branch,
            )?;

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
                        std::fs::copy(&src, &dst).map_err(|e| {
                            format!("Failed to copy {}: {}", item.display(), e)
                        })?;
                    }
                }
            }

            // Store deferred info — first repo as representative
            self.deferred_branch = Some((repos[0].absolute_path.clone(), branch_name.clone()));

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
            let (_, branch_name) = self.deferred_branch.as_ref()
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
            let (_, base_branch) = self.deferred_branch.as_ref()
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
                    return Err(format!("git branch failed in {}: {}", repo.relative_path.display(), stderr.trim()));
                }

                let refspec = format!("{}:{}", branch_name, branch_name);
                let _ = Command::new("git")
                    .args(["fetch", &clone_path.to_string_lossy(), &refspec, "--force"])
                    .current_dir(&repo.absolute_path)
                    .output();

                self.created_branches.push((repo.absolute_path.clone(), branch_name));
            }
            self.deferred_branch = None;
        }

        ProjectLayout::NoGit => {}
    }

    Ok(())
}
```

Add `deferred_branch` field to `ProjectMount` struct:

```rust
/// Deferred branch info: (repo_path, branch_name) — set when setup_deferred() is used.
/// Consumed by create_source_branch_and_fetch().
pub deferred_branch: Option<(PathBuf, String)>,
```

Initialize it in `detect()` (both return paths) and in `ProjectMount` construction:
```rust
deferred_branch: None,
```

**Step 4: Run tests**

Run: `cargo test --features cli project::tests`
Expected: all tests pass (existing + 2 new)

**Step 5: Commit**

```bash
git add src/project.rs
git commit -m "feat: add setup_deferred() and create_source_branch_and_fetch() for lazy branch creation"
```

---

### Task 5: Wire settings into `App` and modify `sync_project_commits()`

**Files:**
- Modify: `src/tui/app.rs`

**Step 1: Add UserSettings to App and sync_override to AgentPanel**

Add to `AgentPanel` struct (after `base_commit` field):

```rust
/// Per-panel sync override. Takes priority over global settings.
/// None = use global, Some(true) = force on, Some(false) = force off.
pub sync_override: Option<bool>,
```

Initialize in `AgentPanel::new()`:
```rust
sync_override: None,
```

Add to `App` struct (after `sidebar_tick_counter`):

```rust
/// Persistent user settings (loaded from ~/.nanosandbox/config.toml).
pub settings: crate::settings::UserSettings,
```

Initialize in `App::new()`:
```rust
settings: crate::settings::UserSettings::load(),
```

**Step 2: Modify `sync_project_commits()` to respect settings**

Replace the existing sync logic in `sync_project_commits()`. After detecting a new commit (the `if panel.last_known_head.as_deref() == Some(&current_head) { continue; }` check), change the behavior based on settings:

```rust
// Determine if auto-sync is active for this panel.
let auto_sync = panel.sync_override
    .unwrap_or(self.settings.gitsync.auto_sync);

// Get commit info for notification.
let subject = std::process::Command::new("git")
    .args(["log", "--format=%s", "-1"])
    .current_dir(wt_base)
    .output()
    .ok()
    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    .unwrap_or_default();
let short_sha = &current_head[..7.min(current_head.len())];

if auto_sync {
    // Fetch from clone to source (existing behavior).
    let (source_path, branch_name) = match pm.created_branches.first() {
        Some((src, branch)) => (src.clone(), branch.clone()),
        None => {
            // If branches not yet created (deferred setup), just notify.
            if self.settings.gitsync.notify_on_commit {
                notifications.push((
                    panel_idx,
                    format!("New commit {}: {} (use /gitsync now to sync)", short_sha, subject),
                ));
            }
            panel.last_known_head = Some(current_head);
            continue;
        }
    };

    let refspec = format!("{}:{}", branch_name, branch_name);
    let fetch_ok = std::process::Command::new("git")
        .args(["fetch", &wt_base.to_string_lossy(), &refspec, "--force"])
        .current_dir(&source_path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if fetch_ok {
        notifications.push((
            panel_idx,
            format!("Synced {} to {}: {}", short_sha, branch_name, subject),
        ));
    }
} else if self.settings.gitsync.notify_on_commit {
    // Notify only — don't fetch.
    notifications.push((
        panel_idx,
        format!("New commit {}: {} (use /gitsync now to sync)", short_sha, subject),
    ));
}

panel.last_known_head = Some(current_head);
```

**Step 3: Run full test suite**

Run: `cargo test --features cli`
Expected: all tests pass

**Step 4: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat: wire UserSettings into App, respect sync settings in sync_project_commits()"
```

---

### Task 6: Wire settings into sandbox creation (`run.rs`) — use `setup_deferred` when sync is off

**Files:**
- Modify: `src/sandbox.rs` (check how `ProjectMount` is called)
- Modify: `src/tui/run.rs` (pass settings to sandbox creation flow)

First check how sandbox.rs calls project mount setup. The sandbox needs to know whether to use `setup()` or `setup_deferred()`. The simplest approach: add a `deferred` flag to `ProjectConfig` or pass it through the sandbox creation flow.

**Step 1: Add `auto_sync` to `ProjectConfig`**

In `src/config.rs`, add to `ProjectConfig`:
```rust
/// Whether auto-sync is enabled. When false, clones are created without source branches.
#[serde(default)]
pub auto_sync: bool,
```

Update `SandboxConfigBuilder::build()` to pass `auto_sync` from settings by having the TUI call `builder.project_auto_sync(settings.gitsync.auto_sync)` or by setting it on the config after build.

**Step 2: In `src/sandbox.rs` where `ProjectMount::setup()` is called**, switch between `setup()` and `setup_deferred()` based on `config.project.auto_sync`.

**Step 3: In `src/tui/run.rs`'s `add_agent()` function**, read `app.settings.gitsync.auto_sync` and set it on the sandbox config before spawning.

**Step 4: Run tests**

Run: `cargo test --features cli`
Expected: all tests pass

**Step 5: Commit**

```bash
git add src/config.rs src/sandbox.rs src/tui/run.rs
git commit -m "feat: use setup_deferred() when auto_sync is off in sandbox creation"
```

---

### Task 7: Handle `/gitsync` command in `run.rs`

**Files:**
- Modify: `src/tui/run.rs`

**Step 1: Add GitSync command handling in `handle_command()`**

Add a new match arm for `Command::GitSync` in `handle_command()` (after `Command::Diff`):

```rust
Command::GitSync { action } => {
    let panel_idx = app.focused_panel;
    match action.as_deref() {
        None => {
            // Show sync status
            let auto = app.panels.get(panel_idx)
                .and_then(|p| p.sync_override)
                .unwrap_or(app.settings.gitsync.auto_sync);
            let status_label = if auto { "ON (unsafe)" } else { "OFF (safe)" };
            let has_branch = app.panels.get(panel_idx)
                .and_then(|p| p.project_mount.as_ref())
                .map(|pm| !pm.created_branches.is_empty())
                .unwrap_or(false);
            let branch_info = if has_branch {
                app.panels.get(panel_idx)
                    .and_then(|p| p.project_mount.as_ref())
                    .and_then(|pm| pm.created_branches.first())
                    .map(|(_, b)| format!("Branch: {}", b))
                    .unwrap_or_default()
            } else {
                "No source branch created yet".to_string()
            };
            let msg = format!(
                "Git sync: {}\nNotify on commit: {}\n{}",
                status_label,
                if app.settings.gitsync.notify_on_commit { "ON" } else { "OFF" },
                branch_info,
            );
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                panel.chat_history.push(ChatMessage {
                    role: MessageRole::System,
                    content: msg,
                });
            }
        }
        Some("on") => {
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                panel.sync_override = Some(true);
                panel.chat_history.push(ChatMessage {
                    role: MessageRole::System,
                    content: "Auto-sync ENABLED for this panel.\n\
                              WARNING: Agent commits will be fetched to your local branch automatically.\n\
                              This can be unsafe — use /gitsync off to disable.".to_string(),
                });
                // Create source branch if deferred
                if let Some(ref mut pm) = panel.project_mount {
                    if pm.created_branches.is_empty() {
                        if let Err(e) = pm.create_source_branch_and_fetch() {
                            panel.chat_history.push(ChatMessage {
                                role: MessageRole::System,
                                content: format!("Failed to create source branch: {}", e),
                            });
                        }
                    }
                }
            }
        }
        Some("off") => {
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                panel.sync_override = Some(false);
                panel.chat_history.push(ChatMessage {
                    role: MessageRole::System,
                    content: "Auto-sync DISABLED for this panel.".to_string(),
                });
            }
        }
        Some("now") => {
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                if let Some(ref mut pm) = panel.project_mount {
                    // Create source branch if deferred
                    if pm.created_branches.is_empty() {
                        if let Err(e) = pm.create_source_branch_and_fetch() {
                            panel.chat_history.push(ChatMessage {
                                role: MessageRole::System,
                                content: format!("Failed to create source branch: {}", e),
                            });
                            return;
                        }
                    }
                    // Fetch current state
                    if let Some(ref wt_base) = pm.worktree_base {
                        if let Some((source, branch)) = pm.created_branches.first() {
                            let refspec = format!("{}:{}", branch, branch);
                            let ok = std::process::Command::new("git")
                                .args(["fetch", &wt_base.to_string_lossy(), &refspec, "--force"])
                                .current_dir(source)
                                .output()
                                .map(|o| o.status.success())
                                .unwrap_or(false);
                            let msg = if ok {
                                format!("Synced to branch '{}'.", branch)
                            } else {
                                "Sync failed. Check clone state.".to_string()
                            };
                            panel.chat_history.push(ChatMessage {
                                role: MessageRole::System,
                                content: msg,
                            });
                        }
                    }
                } else {
                    panel.chat_history.push(ChatMessage {
                        role: MessageRole::System,
                        content: "No project mount for this panel.".to_string(),
                    });
                }
            }
        }
        _ => {} // parse_gitsync already validates
    }
}
```

**Step 2: Run build + tests**

Run: `cargo build --features cli && cargo test --features cli`
Expected: passes

**Step 3: Commit**

```bash
git add src/tui/run.rs
git commit -m "feat: handle /gitsync command (status, on, off, now)"
```

---

### Task 8: Handle `/open` command with suspend-and-launch

**Files:**
- Modify: `src/tui/run.rs`

**Step 1: Add Open command handling**

Add match arm for `Command::Open` in `handle_command()`:

```rust
Command::Open { tool } => {
    let panel_idx = app.focused_panel;
    let clone_path = app.panels.get(panel_idx)
        .and_then(|p| p.project_mount.as_ref())
        .and_then(|pm| pm.worktree_base.clone());

    let clone_path = match clone_path {
        Some(p) => p,
        None => {
            let msg = ChatMessage {
                role: MessageRole::System,
                content: "No project clone for this panel.".to_string(),
            };
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                panel.chat_history.push(msg);
            }
            return;
        }
    };

    let editor_pref = tool.as_deref().unwrap_or(&app.settings.tools.editor);

    // Handle custom command
    if editor_pref == "custom" {
        if let Some(ref cmd_template) = app.settings.tools.custom_command {
            let cmd = cmd_template.replace("{path}", &clone_path.to_string_lossy());
            let parts: Vec<&str> = cmd.split_whitespace().collect();
            if let Some((bin, args)) = parts.split_first() {
                let _ = std::process::Command::new(bin)
                    .args(args)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                panel.chat_history.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Opened with custom command."),
                });
            }
            return;
        }
    }

    let resolved = crate::settings::resolve_tool(editor_pref);

    match resolved {
        Some((binary, true)) => {
            // TUI tool: suspend-and-launch
            // This is returned as a special signal. We need terminal access.
            // Store the pending open action and handle it in the event loop.
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                panel.chat_history.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Opening in {}...", binary),
                });
            }
            // Send a special event to trigger suspend-and-launch
            let _ = tx.send(AppEvent::OpenTuiTool {
                binary: binary.to_string(),
                path: clone_path,
            });
        }
        Some((binary, false)) => {
            // GUI tool: fire-and-forget
            let _ = std::process::Command::new(binary)
                .arg(&clone_path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                panel.chat_history.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!("Opened in {}.", binary),
                });
            }
        }
        None => {
            if let Some(panel) = app.panels.get_mut(panel_idx) {
                panel.chat_history.push(ChatMessage {
                    role: MessageRole::System,
                    content: format!(
                        "No tool '{}' found. Install gitui, lazygit, or VS Code.\n\
                         Or set editor in ~/.nanosandbox/config.toml",
                        editor_pref,
                    ),
                });
            }
        }
    }
}
```

**Step 2: Add `OpenTuiTool` event variant**

In `src/tui/event.rs`, add to `AppEvent`:
```rust
/// Open a TUI tool (suspend terminal, launch tool, resume on exit).
OpenTuiTool { binary: String, path: std::path::PathBuf },
```

**Step 3: Handle `OpenTuiTool` in the main event loop**

In `run.rs`, add handler in the main event match (after `AppEvent::Tick`):

```rust
AppEvent::OpenTuiTool { binary, path } => {
    // Suspend TUI: leave alternate screen, disable raw mode
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);

    // Restore stderr for the tool
    if saved_stderr >= 0 {
        unsafe { libc::dup2(saved_stderr, libc::STDERR_FILENO); }
    }

    // Launch tool and wait for exit
    let tool_args: Vec<&str> = match binary.as_str() {
        "gitui" => vec!["-d", &path.to_string_lossy()],
        "lazygit" => vec!["-p", &path.to_string_lossy()],
        "tig" => vec![],
        _ => vec![&path.to_string_lossy()],
    };
    // For tig, we need to cd to the path
    let mut cmd = std::process::Command::new(&binary);
    if binary == "tig" {
        cmd.current_dir(&path);
    } else {
        cmd.args(&tool_args);
    }
    let _ = cmd.status(); // blocks until tool exits

    // Redirect stderr back to /dev/null
    let dev_null = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY) };
    if dev_null >= 0 {
        unsafe {
            libc::dup2(dev_null, libc::STDERR_FILENO);
            libc::close(dev_null);
        }
    }

    // Resume TUI: enter alternate screen, enable raw mode
    let _ = enable_raw_mode();
    let _ = execute!(terminal.backend_mut(), EnterAlternateScreen);
    terminal.clear()?;
}
```

Note: the `path.to_string_lossy()` args need to be stored in a variable before passing as &str due to lifetime. Adjust accordingly in implementation.

**Step 4: Run build + tests**

Run: `cargo build --features cli && cargo test --features cli`
Expected: passes

**Step 5: Commit**

```bash
git add src/tui/run.rs src/tui/event.rs
git commit -m "feat: handle /open command with suspend-and-launch for TUI tools"
```

---

### Task 9: Update renderer for sync indicator + update /help text

**Files:**
- Modify: `src/tui/renderer.rs`
- Modify: `src/tui/run.rs` (help text)

**Step 1: Add sync indicator to sandbox list**

In `render_sandbox_list()`, after the existing `sid` and `focus_marker`, add sync status:

```rust
let sync_label = if panel.project_mount.is_some() {
    if panel.sync_override.unwrap_or(false) || /* check global */ false {
        Span::styled(" [sync]", Style::new().fg(Color::Green))
    } else {
        Span::styled(" [clone]", Style::new().fg(Color::DarkGray))
    }
} else {
    Span::raw("")
};
```

Add `sync_label` to the `Line::from(vec![...])` call.

Note: The renderer doesn't have access to `app.settings`, but it has `app` so it can read `app.settings.gitsync.auto_sync`. Check each panel's override and fall back to the global setting.

**Step 2: Update /help text in handle_command()**

Add to the help string:
```
"  /gitsync [on|off|now]         Git sync control\n",
"  /open [tool]                  Open clone in external tool\n",
```

**Step 3: Run build**

Run: `cargo build --features cli`
Expected: passes

**Step 4: Commit**

```bash
git add src/tui/renderer.rs src/tui/run.rs
git commit -m "feat: add sync indicator to sidebar and update /help text"
```

---

### Task 10: Full build and test verification

**Step 1: Full build**

Run: `cargo build --features cli`
Expected: compiles cleanly

**Step 2: Full test suite**

Run: `cargo test --features cli`
Expected: all tests pass (173+ unit, 27 integration, 26 CLI)

**Step 3: Clippy check**

Run: `cargo clippy --features cli`
Expected: no warnings

**Step 4: Final commit if any fixups needed**

```bash
git add -A
git commit -m "chore: fix clippy warnings in gitsync feature"
```
