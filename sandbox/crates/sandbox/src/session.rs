//! Session persistence for runtime.
//!
//! Allows users to quit the TUI and later resume where they left off.
//! A session captures the state of all agent panels (project clones,
//! branches, agent state directories) so that a new VM can be booted
//! with the same filesystem state.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Schema version for forward compatibility.
pub const SESSION_VERSION: u32 = 1;

/// Persisted session state for one project directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Schema version.
    pub version: u32,
    /// When the session was first created.
    pub created_at: DateTime<Utc>,
    /// When the session was last suspended (updated on each /quit).
    pub updated_at: DateTime<Utc>,
    /// The canonical project path this session is bound to.
    pub project_path: PathBuf,
    /// SHA-256 hex digest of the sandbox.yml content at session creation time.
    /// Used to detect config changes between sessions.
    pub config_hash: String,
    /// Per-panel state.
    pub panels: Vec<SessionPanel>,
}

/// Persisted state for a single agent panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPanel {
    /// Agent type key ("claude", "codex", "goose", "cursor", etc.).
    pub agent_name: String,
    /// Display name shown in the panel title (falls back to agent_name).
    pub display_name: Option<String>,
    /// Short sandbox ID (first 8 chars of UUID) — used to locate clone dir.
    pub sandbox_short_id: String,
    /// Full sandbox config needed to recreate the sandbox.
    pub config: crate::config::AgentSandboxConfig,
    /// Absolute host path to the project clone directory.
    /// `None` if no project was mounted.
    pub clone_path: Option<PathBuf>,
    /// Branches created in source repos, as `(source_repo_path, branch_name)` pairs.
    pub branches: Vec<(PathBuf, String)>,
    /// Whether auto/headless mode was enabled.
    pub auto_mode: bool,
    /// Agent permission level.
    #[serde(default)]
    pub permissions: crate::config::Permissions,
    /// Agent type enum (serialized as string).
    #[serde(default)]
    pub agent_type: Option<crate::config::AgentType>,
    /// Model identifier.
    #[serde(default)]
    pub model: Option<String>,
    /// Task prompt for headless/auto mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Environment variable **keys** that were set for this panel.
    /// Values are NOT stored (security). On resume, values are re-read from host env.
    pub env_keys: Vec<String>,
    /// Whether the panel was visible in the grid.
    pub visible: bool,
    /// Whether the user actually interacted with the agent (sent keystrokes).
    /// When false, the agent had no conversation so resume flags (--continue)
    /// should not be passed.
    #[serde(default)]
    pub had_interaction: bool,
    /// Explicit agent-native session ID selected by the user for resume.
    /// When set, CLI should prefer this ID over generic latest-continue flags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_agent_session_id: Option<String>,
}

/// Issues discovered when validating a saved session.
#[derive(Debug)]
pub struct SessionIssue {
    /// Panel index this issue relates to (or None for session-level).
    pub panel_idx: Option<usize>,
    /// Human-readable description of the issue.
    pub message: String,
    /// Whether the session can still be resumed despite this issue.
    pub recoverable: bool,
}

/// Session entry for listing session history.
#[derive(Debug, Clone)]
pub struct SessionListEntry {
    /// Unique session identifier.
    pub id: String,
    /// Session payload.
    pub session: Session,
}

impl std::fmt::Display for SessionIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(idx) = self.panel_idx {
            write!(f, "Panel {}: {}", idx, self.message)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

// ── Path helpers ─────────────────────────────────────────────────────

/// Compute the session directory for a given project path.
///
/// Returns `~/.nanosandbox/sessions/<path-hash>/` where `<path-hash>` is a
/// 16-character hex hash of the canonical project path.
pub fn session_dir(project_path: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    project_path.to_string_lossy().as_ref().hash(&mut hasher);
    let hash = format!("{:016x}", hasher.finish());

    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".nanosandbox")
        .join("sessions")
        .join(hash)
}

const LEGACY_SESSION_FILE: &str = "session.json";

/// Path to the legacy session metadata file within a session directory.
fn legacy_session_file(dir: &Path) -> PathBuf {
    dir.join(LEGACY_SESSION_FILE)
}

/// Path to a session metadata file for a specific session id.
fn session_file_for_id(dir: &Path, session_id: &str) -> PathBuf {
    dir.join(format!("session-{}.json", session_id))
}

fn parse_session_id_from_filename(name: &str) -> Option<String> {
    if name == LEGACY_SESSION_FILE {
        return Some("legacy".to_string());
    }

    if let Some(stripped) = name.strip_prefix("session-") {
        return stripped
            .strip_suffix(".json")
            .map(std::string::ToString::to_string);
    }

    None
}

fn session_file_from_id(dir: &Path, session_id: &str) -> PathBuf {
    if session_id == "legacy" {
        legacy_session_file(dir)
    } else {
        session_file_for_id(dir, session_id)
    }
}

fn generate_session_id() -> String {
    let ts = Utc::now().format("%Y%m%d%H%M%S");
    let short_uuid = Uuid::new_v4().simple().to_string();
    format!("{}-{}", ts, &short_uuid[..8])
}

/// Compute a SHA-256 hex digest of a string (used for config hashing).
pub fn config_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Format a chrono Duration as a human-readable string.
pub fn format_age(dt: &DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(*dt);

    if duration.num_days() > 0 {
        format!("{}d ago", duration.num_days())
    } else if duration.num_hours() > 0 {
        format!("{}h ago", duration.num_hours())
    } else if duration.num_minutes() > 0 {
        format!("{}m ago", duration.num_minutes())
    } else {
        "just now".to_string()
    }
}

// ── Session implementation ───────────────────────────────────────────

impl Session {
    /// Load a session from disk for the given project path.
    ///
    /// Returns `None` if no session file exists or if it cannot be parsed.
    pub fn load(project_path: &Path) -> Option<Self> {
        Self::list(project_path)
            .into_iter()
            .max_by_key(|entry| entry.session.updated_at)
            .map(|entry| entry.session)
    }

    /// Load a specific session from disk by session id.
    ///
    /// Returns `None` if the id does not exist or if the payload is invalid.
    pub fn load_by_id(project_path: &Path, session_id: &str) -> Option<Self> {
        let dir = session_dir(project_path);
        let file = session_file_from_id(&dir, session_id);

        if !file.exists() {
            return None;
        }

        let content = fs::read_to_string(&file).ok()?;
        let session: Session = serde_json::from_str(&content).ok()?;

        if session.version > SESSION_VERSION {
            return None;
        }

        Some(session)
    }

    /// List all persisted sessions for a project (latest first).
    pub fn list(project_path: &Path) -> Vec<SessionListEntry> {
        let dir = session_dir(project_path);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        };

        let mut sessions = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };

            let Some(id) = parse_session_id_from_filename(name) else {
                continue;
            };

            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(session) = serde_json::from_str::<Session>(&content) else {
                continue;
            };

            if session.version > SESSION_VERSION {
                continue;
            }

            sessions.push(SessionListEntry { id, session });
        }

        sessions.sort_by(|a, b| b.session.updated_at.cmp(&a.session.updated_at));
        sessions
    }

    /// Save this session to disk.
    ///
    /// Returns the generated session ID.
    pub fn save(&self) -> Result<String, String> {
        self.save_with_id(None)
    }

    /// Save this session to disk, optionally reusing an existing session id.
    ///
    /// When `session_id` is provided, the corresponding session file is overwritten.
    /// When `None`, a new session id is generated.
    pub fn save_with_id(&self, session_id: Option<&str>) -> Result<String, String> {
        let dir = session_dir(&self.project_path);
        fs::create_dir_all(&dir).map_err(|e| format!("Failed to create session dir: {}", e))?;

        let session_id = session_id
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(std::string::ToString::to_string)
            .unwrap_or_else(generate_session_id);
        let file = session_file_from_id(&dir, &session_id);
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize session: {}", e))?;

        fs::write(&file, content).map_err(|e| format!("Failed to write session file: {}", e))?;

        Ok(session_id)
    }

    /// Delete the session file and optionally the entire session directory.
    pub fn delete(project_path: &Path, remove_all: bool) -> Result<(), String> {
        let dir = session_dir(project_path);

        if remove_all {
            // Remove entire session directory
            if dir.exists() {
                fs::remove_dir_all(&dir)
                    .map_err(|e| format!("Failed to remove session dir: {}", e))?;
            }
        } else {
            // Remove the latest session metadata file only.
            if let Some(latest) = Self::list(project_path).first() {
                let file = session_file_from_id(&dir, &latest.id);
                if file.exists() {
                    fs::remove_file(&file)
                        .map_err(|e| format!("Failed to remove session file: {}", e))?;
                }
            }
        }

        Ok(())
    }

    /// Validate that the saved session state is consistent and resumable.
    pub fn validate(&self) -> Vec<SessionIssue> {
        let mut issues = Vec::new();

        // Check project path exists
        if !self.project_path.exists() {
            issues.push(SessionIssue {
                panel_idx: None,
                message: format!(
                    "Project path no longer exists: {}",
                    self.project_path.display()
                ),
                recoverable: false,
            });
            return issues;
        }

        for (idx, panel) in self.panels.iter().enumerate() {
            // Check clone directory
            if let Some(ref clone_path) = panel.clone_path {
                if !clone_path.exists() {
                    // Check if the branch still exists in source — we could re-clone
                    let branch_exists = panel.branches.first().is_some_and(|(repo, branch)| {
                        std::process::Command::new("git")
                            .args(["rev-parse", "--verify", &format!("refs/heads/{}", branch)])
                            .current_dir(repo)
                            .output()
                            .map(|o| o.status.success())
                            .unwrap_or(false)
                    });

                    if branch_exists {
                        issues.push(SessionIssue {
                            panel_idx: Some(idx),
                            message: format!(
                                "Clone dir missing but branch exists — will re-clone: {}",
                                clone_path.display()
                            ),
                            recoverable: true,
                        });
                    } else {
                        issues.push(SessionIssue {
                            panel_idx: Some(idx),
                            message: format!(
                                "Clone dir and branch both missing: {}",
                                clone_path.display()
                            ),
                            recoverable: false,
                        });
                    }
                }
            }
        }

        issues
    }

    /// Remove a panel from the session by its short sandbox ID.
    /// Returns true if the panel was found and removed.
    pub fn remove_panel(&mut self, sandbox_short_id: &str) -> bool {
        let before = self.panels.len();
        self.panels
            .retain(|p| p.sandbox_short_id != sandbox_short_id);
        self.panels.len() < before
    }

    /// Check if the session has any panels left.
    pub fn is_empty(&self) -> bool {
        self.panels.is_empty()
    }

    /// Get a summary string for display (e.g., "claude + goose, last active 3h ago").
    pub fn summary(&self) -> String {
        let agents: Vec<&str> = self.panels.iter().map(|p| p.agent_name.as_str()).collect();

        let agents_str = if agents.len() <= 3 {
            agents.join(" + ")
        } else {
            format!("{} agents", agents.len())
        };

        format!(
            "{}, last active {}",
            agents_str,
            format_age(&self.updated_at)
        )
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_session(project_path: &Path) -> Session {
        Session {
            version: SESSION_VERSION,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            project_path: project_path.to_path_buf(),
            config_hash: config_hash("test-config"),
            panels: vec![SessionPanel {
                agent_name: "claude".to_string(),
                display_name: None,
                sandbox_short_id: "abcd1234".to_string(),
                config: crate::config::AgentSandboxConfig {
                    sandbox: runtime::SandboxConfig::builder()
                        .name("test")
                        .image("test:latest")
                        .build(),
                    ..Default::default()
                },
                clone_path: Some(PathBuf::from("/tmp/nanosb-test-clone")),
                branches: vec![(PathBuf::from("/tmp/repo"), "nanosb/abcd1234".to_string())],
                auto_mode: false,
                permissions: crate::config::Permissions::Default,
                agent_type: None,
                model: None,
                prompt: None,
                env_keys: vec!["ANTHROPIC_API_KEY".to_string()],
                visible: true,
                had_interaction: false,
                selected_agent_session_id: None,
            }],
        }
    }

    #[test]
    fn test_session_dir_deterministic() {
        let path = Path::new("/home/user/my-project");
        let dir1 = session_dir(path);
        let dir2 = session_dir(path);
        assert_eq!(dir1, dir2);
    }

    #[test]
    fn test_session_dir_different_paths() {
        let dir1 = session_dir(Path::new("/home/user/project-a"));
        let dir2 = session_dir(Path::new("/home/user/project-b"));
        assert_ne!(dir1, dir2);
    }

    #[test]
    fn test_config_hash_deterministic() {
        let h1 = config_hash("sandbox:\n  image: claude:latest\n");
        let h2 = config_hash("sandbox:\n  image: claude:latest\n");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_config_hash_different_content() {
        let h1 = config_hash("image: claude:latest");
        let h2 = config_hash("image: goose:latest");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_session_serialize_roundtrip() {
        let session = make_test_session(Path::new("/tmp/test-project"));
        let json = serde_json::to_string_pretty(&session).unwrap();
        let restored: Session = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.version, session.version);
        assert_eq!(restored.project_path, session.project_path);
        assert_eq!(restored.config_hash, session.config_hash);
        assert_eq!(restored.panels.len(), 1);
        assert_eq!(restored.panels[0].agent_name, "claude");
        assert_eq!(restored.panels[0].sandbox_short_id, "abcd1234");
    }

    #[test]
    fn test_session_save_and_load() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_path = tmp.path().join("my-project");
        std::fs::create_dir_all(&project_path).unwrap();

        let mut session = make_test_session(&project_path);
        // Manually set clone_path to None so validation doesn't complain
        session.panels[0].clone_path = None;

        session.save().unwrap();
        let loaded = Session::load(&project_path).unwrap();

        assert_eq!(loaded.version, SESSION_VERSION);
        assert_eq!(loaded.panels.len(), 1);
        assert_eq!(loaded.panels[0].agent_name, "claude");

        // Cleanup
        Session::delete(&project_path, true).unwrap();
        assert!(Session::load(&project_path).is_none());
    }

    #[test]
    fn test_list_and_load_by_id() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_path = tmp.path().join("my-project");
        std::fs::create_dir_all(&project_path).unwrap();

        let mut first = make_test_session(&project_path);
        first.panels[0].clone_path = None;
        first.updated_at = Utc::now() - chrono::Duration::minutes(5);
        first.save().unwrap();

        let mut second = make_test_session(&project_path);
        second.panels[0].clone_path = None;
        second.updated_at = Utc::now();
        second.save().unwrap();

        let listed = Session::list(&project_path);
        assert_eq!(listed.len(), 2);

        // list() returns latest first
        assert!(listed[0].session.updated_at >= listed[1].session.updated_at);

        let id = listed[0].id.clone();
        let loaded = Session::load_by_id(&project_path, &id).unwrap();
        assert_eq!(loaded.version, SESSION_VERSION);
        assert_eq!(loaded.panels.len(), 1);

        Session::delete(&project_path, true).unwrap();
    }

    #[test]
    fn test_save_with_id_overwrites_existing_session_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_path = tmp.path().join("my-project");
        std::fs::create_dir_all(&project_path).unwrap();

        let mut first = make_test_session(&project_path);
        first.panels[0].clone_path = None;
        let id = first.save().unwrap();

        let mut second = make_test_session(&project_path);
        second.panels[0].clone_path = None;
        second.panels[0].agent_name = "codex".to_string();
        let reused_id = second.save_with_id(Some(&id)).unwrap();

        assert_eq!(reused_id, id);
        let listed = Session::list(&project_path);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);

        let loaded = Session::load_by_id(&project_path, &listed[0].id).unwrap();
        assert_eq!(loaded.panels[0].agent_name, "codex");

        Session::delete(&project_path, true).unwrap();
    }

    #[test]
    fn test_remove_panel() {
        let mut session = make_test_session(Path::new("/tmp/test"));
        assert_eq!(session.panels.len(), 1);
        assert!(session.remove_panel("abcd1234"));
        assert!(session.is_empty());
        assert!(!session.remove_panel("nonexistent"));
    }

    #[test]
    fn test_summary() {
        let session = make_test_session(Path::new("/tmp/test"));
        let summary = session.summary();
        assert!(summary.contains("claude"));
        assert!(summary.contains("ago") || summary.contains("just now"));
    }

    #[test]
    fn test_format_age() {
        let now = Utc::now();
        assert_eq!(format_age(&now), "just now");

        let two_hours_ago = now - chrono::Duration::hours(2);
        assert_eq!(format_age(&two_hours_ago), "2h ago");

        let three_days_ago = now - chrono::Duration::days(3);
        assert_eq!(format_age(&three_days_ago), "3d ago");
    }

    #[test]
    fn test_delete_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_path = tmp.path().join("my-project");
        std::fs::create_dir_all(&project_path).unwrap();

        let mut session = make_test_session(&project_path);
        session.panels[0].clone_path = None;
        session.save().unwrap();

        let sess_dir = session_dir(&project_path);

        // Delete with remove_agent_state=true removes entire session dir
        Session::delete(&project_path, true).unwrap();
        assert!(!sess_dir.exists());
    }
}
