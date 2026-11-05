//! Mount planner: from `AgentSandboxConfig` + agent type, compute the boot mount plan.
//!
//! # Mount layout
//!
//! ```text
//! ~/.nanosandbox/sandboxes/{name}/
//!   config/   → RO-mounted at various guest paths (MCP configs, skills, prompts)
//!   state/    → RW-mounted at agent state dirs
//! workspace/  → RW-mounted at the guest home (project mount)
//! ```
//!
//! # Mount ordering and nesting
//!
//! The project mount is emitted FIRST, before any mount that lives under it.
//! When the project is mounted at the guest home (`/home/developer`), the
//! per-agent state/config mounts (`.claude`, `.codex`, ...) are emitted as
//! separate mounts on top of it. The guest init applies `KRUN_EXTRA_MOUNTS` in
//! array order, and a Linux nested virtiofs mount is visible on top of its
//! parent — so agent state stays in `~/.nanosandbox/sandboxes/{name}/state/`,
//! NEVER inside the project/repo (which would pollute shared-mode repos).
//!
//! # Per-agent state directories (RW)
//!
//! | Agent   | State dirs                                                      |
//! |---------|-----------------------------------------------------------------|
//! | Claude  | ~/.claude                                                       |
//! | Codex   | ~/.codex, ~/.agents                                              |
//! | Goose   | ~/.config/goose, ~/.local/share/goose                            |
//! | Cursor  | ~/.cursor                                                       |
//! | All     | ~/.nanosandbox (shared agent metadata)                           |
//!
//! # Per-agent config directories (RO)
//!
//! | Agent   | Config dirs                                                     |
//! |---------|-----------------------------------------------------------------|
//! | Claude  | ~/.claude (MCP settings.json merged with state)                  |
//! | Codex   | ~/.codex (AGENTS.md), ~/.agents/skills/                          |
//! | Goose   | ~/.config/goose (config.yaml)                                    |
//! | Cursor  | ~/.cursor/rules/                                                 |

use std::path::{Path, PathBuf};

use crate::config::{AgentSandboxConfig, AgentType};

/// A planned mount for virtiofs delivery.
///
/// This is a local type with a `readonly` field. Convert to `runtime::Mount`
/// via `Into<runtime::Mount>`.
#[derive(Debug, Clone)]
pub struct PlannedMount {
    /// Host path (under `~/.nanosandbox/sandboxes/{name}/` or user-specified).
    pub host_path: PathBuf,
    /// Guest path (inside the VM, e.g. `/home/developer`, `/home/developer/.claude`).
    pub guest_path: String,
    /// Whether the mount is read-only.
    pub readonly: bool,
}

impl From<PlannedMount> for runtime::Mount {
    fn from(pm: PlannedMount) -> Self {
        let mut mount = runtime::Mount {
            mount_type: runtime::MountType::VirtioFs,
            ..runtime::Mount::bind(pm.host_path, pm.guest_path)
        };
        mount.readonly = pm.readonly;
        mount
    }
}

/// Per-agent state directory definitions.
fn agent_state_dirs(agent_type: AgentType) -> Vec<&'static str> {
    match agent_type {
        AgentType::Claude => vec!["/home/developer/.claude"],
        AgentType::Codex => vec!["/home/developer/.codex", "/home/developer/.agents"],
        AgentType::Goose => vec![
            "/home/developer/.config/goose",
            "/home/developer/.local/share/goose",
        ],
        AgentType::Cursor => vec!["/home/developer/.cursor"],
        AgentType::Opencode => vec!["/home/developer/.config/opencode"],
        AgentType::Aider => vec!["/home/developer/.aider"],
        AgentType::Gemini => vec!["/home/developer/.gemini"],
        AgentType::Crush => vec!["/home/developer/.config/crush"],
    }
}

/// Per-agent config directory definitions (guest paths).
/// These are subdirectories under the sandbox config dir that get mounted RO.
fn agent_config_guest_paths(agent_type: AgentType) -> Vec<(&'static str, &'static str)> {
    // Returns (guest_path, relative_subdir_under_config)
    match agent_type {
        AgentType::Claude => vec![("/home/developer/.claude", "claude")],
        AgentType::Codex => vec![
            ("/home/developer/.codex", "codex"),
            ("/home/developer/.agents", "agents"),
        ],
        AgentType::Goose => vec![("/home/developer/.config/goose", "goose")],
        AgentType::Cursor => vec![("/home/developer/.cursor", "cursor")],
        AgentType::Opencode => vec![("/home/developer/.config/opencode", "opencode")],
        AgentType::Aider => vec![("/home/developer/.aider", "aider")],
        AgentType::Gemini => vec![("/home/developer/.gemini", "gemini")],
        AgentType::Crush => vec![("/home/developer/.config/crush", "crush")],
    }
}

/// Map a config file's relative path (e.g. "claude/settings.json") to its
/// location under the sandbox state dir (e.g. ".claude/settings.json").
pub fn merged_config_path(relative_path: &str) -> Option<String> {
    let (subdir, rest) = relative_path.split_once('/')?;
    let state_dir = match subdir {
        "claude" => ".claude",
        "codex" => ".codex",
        "agents" => ".agents",
        "goose" => ".config_goose",
        "cursor" => ".cursor",
        "opencode" => ".config_opencode",
        "aider" => ".aider",
        "gemini" => ".gemini",
        "crush" => ".config_crush",
        _ => return None,
    };
    Some(format!("{}/{}", state_dir, rest))
}

/// The guest user's home directory. Pure agent images ship no `developer`
/// user, so the runtime creates it at this fixed non-root home.
pub const GUEST_HOME: &str = "/home/developer";

/// Mount planner: computes the full set of virtiofs mounts for a sandbox.
pub struct MountPlanner;

impl MountPlanner {
    /// Compute the mount plan for a sandbox.
    ///
    /// * `config` - The agent sandbox config.
    /// * `agent_type` - The resolved agent type.
    /// * `sandbox_dir` - Host path `~/.nanosandbox/sandboxes/{name}/`.
    /// * `workspace_host_path` - Optional host path for the workspace mount.
    ///   If `None`, the workspace mount is omitted (no project configured).
    pub fn plan(
        config: &AgentSandboxConfig,
        agent_type: &AgentType,
        sandbox_dir: &Path,
        workspace_host_path: Option<&Path>,
    ) -> Vec<PlannedMount> {
        let mut mounts = Vec::new();

        // The workspace mount point defaults to the guest home. When a project
        // is configured its `mount_point` wins.
        let workspace_guest_path = config
            .sandbox
            .project
            .as_ref()
            .map(|p| p.mount_point.clone())
            .unwrap_or_else(|| GUEST_HOME.to_string());

        // Determine the workspace host path (project clone or config path).
        let workspace_host = workspace_host_path
            .map(|p| p.to_path_buf())
            .or_else(|| config.sandbox.project.as_ref().map(|p| p.path.clone()));

        // 1. Workspace mount (RW)
        if let Some(ws_host) = &workspace_host {
            mounts.push(PlannedMount {
                host_path: ws_host.clone(),
                guest_path: workspace_guest_path.clone(),
                readonly: false,
            });
        }

        // 2. Agent state directories (RW) — each gets its own mount. These are
        //    emitted AFTER the workspace mount so the guest init mounts them on
        //    TOP of the project at the home (nested virtiofs mounts) instead of
        //    the project shadowing them.
        let state_base = sandbox_dir.join("state");
        for guest_dir in agent_state_dirs(*agent_type) {
            let dir_name = guest_dir
                .trim_start_matches("/home/developer/")
                .replace('/', "_");
            let host_state_dir = state_base.join(&dir_name);
            mounts.push(PlannedMount {
                host_path: host_state_dir,
                guest_path: guest_dir.to_string(),
                readonly: false,
            });
        }

        // 3. Config directories. Generated config files are merged into the
        //    state mounts above (see `merged_config_path`), so config mounts
        //    are only added for guest paths that are not already mounted.
        let config_base = sandbox_dir.join("config");
        for (guest_path, subdir) in agent_config_guest_paths(*agent_type) {
            if mounts.iter().any(|m| m.guest_path == guest_path) {
                continue;
            }
            let host_config_dir = config_base.join(subdir);
            mounts.push(PlannedMount {
                host_path: host_config_dir,
                guest_path: guest_path.to_string(),
                readonly: true,
            });
        }

        // 4. Shared agent metadata dir (RW) — ~/.nanosandbox inside guest
        mounts.push(PlannedMount {
            host_path: sandbox_dir.join("state").join("nanosandbox"),
            guest_path: "/home/developer/.nanosandbox".to_string(),
            readonly: false,
        });

        mounts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentType;

    fn make_config(agent_type: Option<AgentType>) -> AgentSandboxConfig {
        AgentSandboxConfig {
            sandbox: runtime::SandboxConfig {
                name: "test".to_string(),
                image: "test:latest".to_string(),
                ..runtime::SandboxConfig::default()
            },
            agent_type,
            ..AgentSandboxConfig::default()
        }
    }

    #[test]
    fn test_plan_claude_mounts() {
        let config = make_config(Some(AgentType::Claude));
        let sandbox_dir = Path::new("/tmp/.nanosandbox/sandboxes/test");
        let mounts = MountPlanner::plan(&config, &AgentType::Claude, sandbox_dir, None);

        // Claude: merged state+config mount + shared nanosandbox
        assert!(
            mounts.len() >= 2,
            "claude should have at least 2 mounts, got {}",
            mounts.len()
        );

        let claude_mounts: Vec<_> = mounts
            .iter()
            .filter(|m| m.guest_path == "/home/developer/.claude")
            .collect();
        assert_eq!(claude_mounts.len(), 1, "exactly one ~/.claude mount");
        assert!(
            !claude_mounts[0].readonly,
            "merged ~/.claude mount should be RW"
        );

        let ns_mount = mounts
            .iter()
            .find(|m| m.guest_path == "/home/developer/.nanosandbox");
        assert!(ns_mount.is_some(), "should have nanosandbox state mount");
        assert!(
            !ns_mount.unwrap().readonly,
            "nanosandbox mount should be RW"
        );
    }

    #[test]
    fn test_plan_goose_mounts() {
        let config = make_config(Some(AgentType::Goose));
        let sandbox_dir = Path::new("/tmp/.nanosandbox/sandboxes/test");
        let mounts = MountPlanner::plan(&config, &AgentType::Goose, sandbox_dir, None);

        // Goose: 2 merged state mounts + nanosandbox shared
        assert!(
            mounts.len() >= 3,
            "goose should have at least 3 mounts, got {}",
            mounts.len()
        );

        let goose_config: Vec<_> = mounts
            .iter()
            .filter(|m| m.guest_path == "/home/developer/.config/goose")
            .collect();
        assert_eq!(goose_config.len(), 1, "exactly one .config/goose mount");
        assert!(!goose_config[0].readonly, "merged goose mount should be RW");

        let goose_state = mounts
            .iter()
            .find(|m| m.guest_path == "/home/developer/.local/share/goose");
        assert!(
            goose_state.is_some(),
            "goose local/share state mount should exist"
        );
        assert!(!goose_state.unwrap().readonly, "state mount should be RW");
    }

    #[test]
    fn test_plan_has_no_duplicate_guest_paths() {
        for agent_type in [
            AgentType::Claude,
            AgentType::Codex,
            AgentType::Goose,
            AgentType::Cursor,
            AgentType::Opencode,
            AgentType::Aider,
            AgentType::Gemini,
            AgentType::Crush,
        ] {
            let config = make_config(Some(agent_type));
            let sandbox_dir = Path::new("/tmp/.nanosandbox/sandboxes/test");
            let mounts = MountPlanner::plan(&config, &agent_type, sandbox_dir, None);
            let mut seen = std::collections::HashSet::new();
            for mount in &mounts {
                assert!(
                    seen.insert(mount.guest_path.clone()),
                    "duplicate guest path {} for {:?}",
                    mount.guest_path,
                    agent_type
                );
            }
        }
    }

    #[test]
    fn test_merged_config_path_mapping() {
        assert_eq!(
            merged_config_path("claude/settings.json").as_deref(),
            Some(".claude/settings.json")
        );
        assert_eq!(
            merged_config_path("goose/config.yaml").as_deref(),
            Some(".config_goose/config.yaml")
        );
        assert_eq!(
            merged_config_path("agents/skills/tdd/SKILL.md").as_deref(),
            Some(".agents/skills/tdd/SKILL.md")
        );
        assert_eq!(merged_config_path("unknown/file"), None);
    }

    #[test]
    fn test_plan_with_workspace_at_home_uses_nested_state_mounts() {
        let config = make_config(Some(AgentType::Claude));
        let sandbox_dir = Path::new("/tmp/.nanosandbox/sandboxes/test");
        let ws = tempfile::tempdir().unwrap();
        let ws_path = ws.path();
        let mounts = MountPlanner::plan(&config, &AgentType::Claude, sandbox_dir, Some(ws_path));

        // The workspace defaults to the guest home. State is NOT folded into it —
        // the project mount comes first and the per-agent state mounts are
        // emitted afterwards so the guest init nests them on top of the home.
        let ws_idx = mounts
            .iter()
            .position(|m| m.guest_path == GUEST_HOME)
            .expect("should have workspace mount at home");
        assert_eq!(mounts[ws_idx].host_path, ws_path);
        assert!(!mounts[ws_idx].readonly, "workspace should be RW");

        // Every nested state mount must exist, live after the workspace mount,
        // and keep its host dir under the sandbox state dir (never the project).
        for state_dir in [
            "/home/developer/.claude",
            "/home/developer/.nanosandbox",
        ] {
            let idx = mounts
                .iter()
                .position(|m| m.guest_path == state_dir)
                .unwrap_or_else(|| panic!("missing nested state mount {}", state_dir));
            assert!(
                idx > ws_idx,
                "state mount {} (index {}) must come after workspace mount (index {})",
                state_dir,
                idx,
                ws_idx
            );
            let host = &mounts[idx].host_path;
            assert!(
                host.starts_with(sandbox_dir),
                "state mount host {:?} must live under the sandbox dir",
                host
            );
        }
        // No state/config dirs folded into the workspace host dir.
        assert!(
            !ws_path.join(".claude").exists(),
            "state must not be written into the workspace"
        );
        assert_eq!(mounts[ws_idx + 1].host_path, sandbox_dir.join("state").join(".claude"));
    }

    #[test]
    fn test_plan_with_workspace_at_custom_mount_point_keeps_state_mounts() {
        let mut config = make_config(Some(AgentType::Claude));
        config.sandbox.project = Some(runtime::ProjectConfig {
            path: PathBuf::from("/home/user/project"),
            branch: None,
            mount_point: "/srv/project".to_string(),
            auto_sync: false,
        });
        let sandbox_dir = Path::new("/tmp/.nanosandbox/sandboxes/test");
        let mounts = MountPlanner::plan(&config, &AgentType::Claude, sandbox_dir, None);

        let ws_mount = mounts.iter().find(|m| m.guest_path == "/srv/project");
        assert!(ws_mount.is_some(), "should have project mount at custom point");
        // State mounts are NOT folded when the workspace is not at the home.
        assert!(
            mounts.iter().any(|m| m.guest_path == "/home/developer/.claude"),
            "state mount kept when workspace is not at home"
        );
    }

    #[test]
    fn test_plan_codex_mounts() {
        let config = make_config(Some(AgentType::Codex));
        let sandbox_dir = Path::new("/tmp/.nanosandbox/sandboxes/test");
        let mounts = MountPlanner::plan(&config, &AgentType::Codex, sandbox_dir, None);

        // Codex: 2 merged state mounts + nanosandbox shared
        assert!(
            mounts.len() >= 3,
            "codex should have at least 3 mounts, got {}",
            mounts.len()
        );

        assert!(
            mounts
                .iter()
                .any(|m| m.guest_path == "/home/developer/.codex"),
            "should have .codex mount"
        );
        assert!(
            mounts
                .iter()
                .any(|m| m.guest_path == "/home/developer/.agents"),
            "should have .agents mount"
        );
    }

    #[test]
    fn test_plan_opencode_mounts() {
        let config = make_config(Some(AgentType::Opencode));
        let sandbox_dir = Path::new("/tmp/.nanosandbox/sandboxes/test");
        let mounts = MountPlanner::plan(&config, &AgentType::Opencode, sandbox_dir, None);

        assert!(
            mounts.len() >= 2,
            "opencode should have at least 2 mounts, got {}",
            mounts.len()
        );
        assert!(
            mounts
                .iter()
                .any(|m| m.guest_path == "/home/developer/.config/opencode"),
            "should have .config/opencode mount"
        );
    }

    #[test]
    fn test_plan_aider_mounts() {
        let config = make_config(Some(AgentType::Aider));
        let sandbox_dir = Path::new("/tmp/.nanosandbox/sandboxes/test");
        let mounts = MountPlanner::plan(&config, &AgentType::Aider, sandbox_dir, None);

        assert!(
            mounts.len() >= 2,
            "aider should have at least 2 mounts, got {}",
            mounts.len()
        );
        assert!(
            mounts
                .iter()
                .any(|m| m.guest_path == "/home/developer/.aider"),
            "should have .aider mount"
        );
    }

    #[test]
    fn test_plan_gemini_mounts() {
        let config = make_config(Some(AgentType::Gemini));
        let sandbox_dir = Path::new("/tmp/.nanosandbox/sandboxes/test");
        let mounts = MountPlanner::plan(&config, &AgentType::Gemini, sandbox_dir, None);

        assert!(
            mounts.len() >= 2,
            "gemini should have at least 2 mounts, got {}",
            mounts.len()
        );
        assert!(
            mounts
                .iter()
                .any(|m| m.guest_path == "/home/developer/.gemini"),
            "should have .gemini mount"
        );
    }

    #[test]
    fn test_plan_crush_mounts() {
        let config = make_config(Some(AgentType::Crush));
        let sandbox_dir = Path::new("/tmp/.nanosandbox/sandboxes/test");
        let mounts = MountPlanner::plan(&config, &AgentType::Crush, sandbox_dir, None);

        assert!(
            mounts.len() >= 2,
            "crush should have at least 2 mounts, got {}",
            mounts.len()
        );
        assert!(
            mounts
                .iter()
                .any(|m| m.guest_path == "/home/developer/.config/crush"),
            "should have .config/crush mount"
        );
    }

    #[test]
    fn test_plan_cursor_mounts() {
        let config = make_config(Some(AgentType::Cursor));
        let sandbox_dir = Path::new("/tmp/.nanosandbox/sandboxes/test");
        let mounts = MountPlanner::plan(&config, &AgentType::Cursor, sandbox_dir, None);

        // Cursor: 1 merged state mount + nanosandbox shared
        assert!(
            mounts.len() >= 2,
            "cursor should have at least 2 mounts, got {}",
            mounts.len()
        );

        assert!(
            mounts
                .iter()
                .any(|m| m.guest_path == "/home/developer/.cursor"),
            "should have .cursor mount"
        );
    }

    #[test]
    fn test_planned_mount_into_runtime() {
        let pm = PlannedMount {
            host_path: PathBuf::from("/host/path"),
            guest_path: "/guest/path".to_string(),
            readonly: true,
        };
        let rm: runtime::Mount = pm.into();
        assert_eq!(rm.host_path, PathBuf::from("/host/path"));
        assert_eq!(rm.container_path, "/guest/path");
        assert!(rm.readonly);
        assert_eq!(rm.mount_type, runtime::MountType::VirtioFs);
    }
}
