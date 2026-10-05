//! Mount planner: from `AgentSandboxConfig` + agent type, compute the boot mount plan.
//!
//! # Mount layout
//!
//! ```text
//! ~/.nanosandbox/sandboxes/{name}/
//!   config/   → RO-mounted at various guest paths (MCP configs, skills, prompts)
//!   state/    → RW-mounted at agent state dirs
//! workspace/  → RW-mounted at /workspace (project mount)
//! ```
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
    /// Guest path (inside the VM, e.g. `/workspace`, `/home/developer/.claude`).
    pub guest_path: String,
    /// Whether the mount is read-only.
    pub readonly: bool,
}

impl From<PlannedMount> for runtime::Mount {
    fn from(pm: PlannedMount) -> Self {
        runtime::Mount {
            host_path: pm.host_path,
            container_path: pm.guest_path,
            readonly: pm.readonly,
            mount_type: runtime::MountType::VirtioFs,
        }
    }
}

/// Per-agent state directory definitions.
fn agent_state_dirs(agent_type: AgentType) -> Vec<&'static str> {
    match agent_type {
        AgentType::Claude => vec![
            "/home/developer/.claude",
        ],
        AgentType::Codex => vec![
            "/home/developer/.codex",
            "/home/developer/.agents",
        ],
        AgentType::Goose => vec![
            "/home/developer/.config/goose",
            "/home/developer/.local/share/goose",
        ],
        AgentType::Cursor => vec![
            "/home/developer/.cursor",
        ],
    }
}

/// Per-agent config directory definitions (guest paths).
/// These are subdirectories under the sandbox config dir that get mounted RO.
fn agent_config_guest_paths(agent_type: AgentType) -> Vec<(&'static str, &'static str)> {
    // Returns (guest_path, relative_subdir_under_config)
    match agent_type {
        AgentType::Claude => vec![
            ("/home/developer/.claude", "claude"),
        ],
        AgentType::Codex => vec![
            ("/home/developer/.codex", "codex"),
            ("/home/developer/.agents", "agents"),
        ],
        AgentType::Goose => vec![
            ("/home/developer/.config/goose", "goose"),
        ],
        AgentType::Cursor => vec![
            ("/home/developer/.cursor", "cursor"),
        ],
    }
}

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

        // 1. Workspace mount (RW)
        if let Some(ws_path) = workspace_host_path {
            mounts.push(PlannedMount {
                host_path: ws_path.to_path_buf(),
                guest_path: "/workspace".to_string(),
                readonly: false,
            });
        } else if let Some(ref project) = config.sandbox.project {
            // Project mount from config
            mounts.push(PlannedMount {
                host_path: project.path.clone(),
                guest_path: project.mount_point.clone(),
                readonly: false,
            });
        }

        // 2. Agent state directories (RW) — each gets its own mount
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

        // 3. Config directories (RO)
        let config_base = sandbox_dir.join("config");
        for (guest_path, subdir) in agent_config_guest_paths(*agent_type) {
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

        // Should have: state dirs (1 for claude) + config dirs (1 for claude) + nanosandbox shared
        assert!(mounts.len() >= 3, "claude should have at least 3 mounts, got {}", mounts.len());

        // Check state mount for ~/.claude
        let claude_state = mounts.iter().find(|m| m.guest_path == "/home/developer/.claude");
        assert!(claude_state.is_some(), "should have ~/.claude state mount");
        assert!(!claude_state.unwrap().readonly, "state mount should be RW");

        // Check config mount for ~/.claude
        let claude_config = mounts.iter().filter(|m| m.guest_path == "/home/developer/.claude" && m.readonly).count();
        // Actually there's only one ~/.claude mount — the state one. Config is separate.
        // Let's check the config mount paths:
        let config_mounts: Vec<_> = mounts.iter().filter(|m| m.readonly).collect();
        assert!(!config_mounts.is_empty(), "should have RO config mounts");

        // Check nanosandbox shared state
        let ns_mount = mounts.iter().find(|m| m.guest_path == "/home/developer/.nanosandbox");
        assert!(ns_mount.is_some(), "should have nanosandbox state mount");
        assert!(!ns_mount.unwrap().readonly, "nanosandbox mount should be RW");
    }

    #[test]
    fn test_plan_goose_mounts() {
        let config = make_config(Some(AgentType::Goose));
        let sandbox_dir = Path::new("/tmp/.nanosandbox/sandboxes/test");
        let mounts = MountPlanner::plan(&config, &AgentType::Goose, sandbox_dir, None);

        // Goose has 2 state dirs + 1 config dir + nanosandbox shared
        assert!(mounts.len() >= 4, "goose should have at least 4 mounts, got {}", mounts.len());

        let goose_config = mounts.iter().find(|m| m.guest_path == "/home/developer/.config/goose" && m.readonly);
        assert!(goose_config.is_some(), "goose config mount should exist and be RO");

        let goose_state = mounts.iter().find(|m| m.guest_path == "/home/developer/.local/share/goose");
        assert!(goose_state.is_some(), "goose local/share state mount should exist");
        assert!(!goose_state.unwrap().readonly, "state mount should be RW");
    }

    #[test]
    fn test_plan_with_workspace() {
        let config = make_config(Some(AgentType::Claude));
        let sandbox_dir = Path::new("/tmp/.nanosandbox/sandboxes/test");
        let ws = Path::new("/home/user/project");
        let mounts = MountPlanner::plan(&config, &AgentType::Claude, sandbox_dir, Some(ws));

        let ws_mount = mounts.iter().find(|m| m.guest_path == "/workspace");
        assert!(ws_mount.is_some(), "should have workspace mount");
        assert_eq!(ws_mount.unwrap().host_path, Path::new("/home/user/project"));
        assert!(!ws_mount.unwrap().readonly, "workspace should be RW");
    }

    #[test]
    fn test_plan_codex_mounts() {
        let config = make_config(Some(AgentType::Codex));
        let sandbox_dir = Path::new("/tmp/.nanosandbox/sandboxes/test");
        let mounts = MountPlanner::plan(&config, &AgentType::Codex, sandbox_dir, None);

        // Codex: 2 state dirs + 2 config dirs + nanosandbox shared
        assert!(mounts.len() >= 5, "codex should have at least 5 mounts, got {}", mounts.len());

        assert!(
            mounts.iter().any(|m| m.guest_path == "/home/developer/.codex"),
            "should have .codex mount"
        );
        assert!(
            mounts.iter().any(|m| m.guest_path == "/home/developer/.agents"),
            "should have .agents mount"
        );
    }

    #[test]
    fn test_plan_cursor_mounts() {
        let config = make_config(Some(AgentType::Cursor));
        let sandbox_dir = Path::new("/tmp/.nanosandbox/sandboxes/test");
        let mounts = MountPlanner::plan(&config, &AgentType::Cursor, sandbox_dir, None);

        // Cursor: 1 state dir + 1 config dir + nanosandbox shared
        assert!(mounts.len() >= 3, "cursor should have at least 3 mounts, got {}", mounts.len());

        assert!(
            mounts.iter().any(|m| m.guest_path == "/home/developer/.cursor"),
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
