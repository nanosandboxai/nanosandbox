//! Agent command builder — ported from the legacy in-VM Go gateway (`buildAgentCommand`).
//!
//! Builds the CLI invocation (binary + args) for each agent type, and resolves
//! the environment map passed to the VM boot.

use crate::config::{AgentType, Permissions};
use std::collections::HashMap;

/// A resolved agent command: binary path + arguments.
#[derive(Debug, Clone)]
pub struct AgentCommand {
    /// The binary to execute (e.g. "claude", "goose", "codex", "cursor-agent").
    pub binary: String,
    /// Arguments to pass to the binary.
    pub args: Vec<String>,
}

/// Agent command builder.
pub struct AgentCommandBuilder;

impl AgentCommandBuilder {
    /// Build the agent CLI command from configuration.
    ///
    /// * `agent_type` - The agent type.
    /// * `prompt` - The initial task prompt (for headless mode) or empty for interactive.
    /// * `auto_mode` - Whether to run in fully autonomous mode.
    /// * `interactive` - Run the agent's interactive UI (omit headless flags).
    /// * `permissions` - The permission level.
    /// * `model` - Optional model override.
    pub fn build(
        agent_type: &AgentType,
        prompt: &str,
        auto_mode: bool,
        interactive: bool,
        permissions: Permissions,
        model: Option<&str>,
    ) -> AgentCommand {
        Self::build_with_session(
            agent_type,
            prompt,
            auto_mode,
            interactive,
            permissions,
            model,
            None,
        )
    }

    /// Build the agent CLI command from configuration, optionally resuming an
    /// agent-native session.
    ///
    /// * `resume_session_id` - When `Some(id)`, the agent is launched with its
    ///   native resume flag (currently only codex honors this; other agents
    ///   ignore it and behave as if `None`).
    pub fn build_with_session(
        agent_type: &AgentType,
        prompt: &str,
        auto_mode: bool,
        interactive: bool,
        permissions: Permissions,
        model: Option<&str>,
        resume_session_id: Option<&str>,
    ) -> AgentCommand {
        let effective_permissions = permissions.effective(auto_mode);

        let interactive = interactive && !auto_mode;

        match agent_type {
            AgentType::Claude => {
                Self::build_claude(prompt, interactive, effective_permissions, model)
            }
            AgentType::Codex => Self::build_codex(
                prompt,
                interactive,
                effective_permissions,
                model,
                resume_session_id,
            ),
            AgentType::Goose => {
                Self::build_goose(prompt, interactive, effective_permissions, model)
            }
            AgentType::Cursor => {
                Self::build_cursor(prompt, auto_mode, interactive, effective_permissions, model)
            }
            AgentType::Opencode => {
                Self::build_opencode(prompt, interactive, effective_permissions, model)
            }
            AgentType::Aider => {
                Self::build_aider(prompt, interactive, effective_permissions, model)
            }
            AgentType::Gemini => {
                Self::build_gemini(prompt, interactive, effective_permissions, model)
            }
            AgentType::Crush => {
                Self::build_crush(prompt, interactive, effective_permissions, model)
            }
        }
    }

    /// Build the environment map for the VM boot.
    ///
    /// This includes:
    /// - Base env vars from the sandbox config
    /// - Agent-specific env vars (e.g. GOOSE_MODE for permissions)
    /// - Secrets are NOT included here — they are passed via `krun_set_env`
    ///   at boot time by the supervisor and never written to disk.
    ///
    /// Returns the env map to pass to the VM boot config.
    pub fn build_env(
        agent_type: &AgentType,
        auto_mode: bool,
        permissions: Permissions,
        base_env: &HashMap<String, String>,
    ) -> HashMap<String, String> {
        let mut env = base_env.clone();
        let effective_permissions = permissions.effective(auto_mode);

        // Agent-specific env vars
        match agent_type {
            AgentType::Goose => match effective_permissions {
                Permissions::AllowAll => {
                    env.insert("GOOSE_MODE".to_string(), "auto".to_string());
                }
                Permissions::AcceptEdits => {
                    env.insert("GOOSE_MODE".to_string(), "smart_approve".to_string());
                }
                Permissions::Default => {}
            },
            AgentType::Cursor => {
                // Cursor's debug log lives at /tmp/cursor-agent-logs-<uid>/ with
                // an absolute `latest.log` symlink the macOS virtiofs passthrough
                // cannot resolve (ELOOP), which crashes the agent right after
                // sign-in. Disable the debug log so it never touches that path.
                env.insert(
                    "CURSOR_AGENT_DISABLE_DEBUG_LOG".to_string(),
                    "1".to_string(),
                );
            }
            AgentType::Claude => {
                // Relocate Claude's config dir to the mounted ~/.claude so the
                // pre-seeded `.claude.json` trust file (see `config_gen`) is found
                // and the workspace-trust dialog is auto-accepted.
                env.insert(
                    "CLAUDE_CONFIG_DIR".to_string(),
                    "/home/developer/.claude".to_string(),
                );
            }
            _ => {}
        }

        env
    }

    // ── Claude Code ─────────────────────────────────────────────────

    fn build_claude(
        prompt: &str,
        interactive: bool,
        permissions: Permissions,
        model: Option<&str>,
    ) -> AgentCommand {
        let mut args = Vec::new();

        if !interactive {
            if !prompt.is_empty() {
                args.push("--print".to_string());
                args.push(prompt.to_string());
            }

            args.push("--verbose".to_string());
            args.push("--output-format".to_string());
            args.push("stream-json".to_string());
            args.push("--include-partial-messages".to_string());
        }

        match permissions {
            Permissions::AllowAll => {
                args.push("--dangerously-skip-permissions".to_string());
            }
            Permissions::AcceptEdits => {
                args.push("--permission-mode".to_string());
                args.push("acceptEdits".to_string());
            }
            Permissions::Default => {}
        }

        if let Some(m) = model {
            args.push("--model".to_string());
            args.push(m.to_string());
        }

        AgentCommand {
            binary: "claude".to_string(),
            args,
        }
    }

    // ── Codex ───────────────────────────────────────────────────────

    fn build_codex(
        prompt: &str,
        interactive: bool,
        permissions: Permissions,
        model: Option<&str>,
        resume_session_id: Option<&str>,
    ) -> AgentCommand {
        let mut args = Vec::new();

        if let Some(id) = resume_session_id {
            if interactive {
                args.push("resume".to_string());
                args.push(id.to_string());
            } else {
                args.push("exec".to_string());
                args.push("--skip-git-repo-check".to_string());

                match permissions {
                    Permissions::AllowAll | Permissions::AcceptEdits => {
                        args.push("--dangerously-bypass-approvals-and-sandbox".to_string());
                    }
                    Permissions::Default => {}
                }

                args.push("--json".to_string());

                if let Some(m) = model {
                    args.push("--model".to_string());
                    args.push(m.to_string());
                }

                args.push("resume".to_string());
                args.push(id.to_string());

                if !prompt.is_empty() {
                    args.push(prompt.to_string());
                }
            }
        } else if !interactive {
            args.push("exec".to_string());
            args.push("--skip-git-repo-check".to_string());

            match permissions {
                Permissions::AllowAll | Permissions::AcceptEdits => {
                    args.push("--dangerously-bypass-approvals-and-sandbox".to_string());
                }
                Permissions::Default => {}
            }

            args.push("--json".to_string());

            if let Some(m) = model {
                args.push("--model".to_string());
                args.push(m.to_string());
            }

            if !prompt.is_empty() {
                args.push(prompt.to_string());
            }
        }

        AgentCommand {
            binary: "codex".to_string(),
            args,
        }
    }

    // ── Goose ───────────────────────────────────────────────────────

    fn build_goose(
        prompt: &str,
        interactive: bool,
        _permissions: Permissions,
        _model: Option<&str>,
    ) -> AgentCommand {
        let mut args = Vec::new();

        if interactive {
            args.push("session".to_string());
        } else if !prompt.is_empty() {
            args.push("run".to_string());
            args.push("--text".to_string());
            args.push(prompt.to_string());
        }
        // Headless with empty prompt: bare `goose` (never `goose configure`).

        AgentCommand {
            binary: "goose".to_string(),
            args,
        }
    }

    // ── Cursor Agent ────────────────────────────────────────────────

    fn build_cursor(
        prompt: &str,
        auto_mode: bool,
        interactive: bool,
        permissions: Permissions,
        model: Option<&str>,
    ) -> AgentCommand {
        let mut args = Vec::new();

        if !interactive && !prompt.is_empty() {
            args.push("--message".to_string());
            args.push(prompt.to_string());
        }

        if interactive {
            // Skip cursor's interactive "Workspace Trust Required" prompt: the
            // workspace is the sandbox's disposable clone, and that prompt can
            // leave the agent stuck when the panel fails to deliver the keypress.
            args.push("--trust".to_string());
        }

        match permissions {
            Permissions::AllowAll => {
                args.push("--force".to_string());
                if auto_mode {
                    args.push("--trust".to_string());
                    args.push("--approve-mcps".to_string());
                }
            }
            Permissions::AcceptEdits => {
                if auto_mode {
                    args.push("--trust".to_string());
                }
            }
            Permissions::Default => {}
        }

        if let Some(m) = model {
            args.push("--model".to_string());
            args.push(m.to_string());
        }

        AgentCommand {
            binary: "cursor-agent".to_string(),
            args,
        }
    }

    // ── OpenCode ────────────────────────────────────────────────────

    fn build_opencode(
        prompt: &str,
        interactive: bool,
        permissions: Permissions,
        model: Option<&str>,
    ) -> AgentCommand {
        let mut args = Vec::new();

        if !interactive {
            args.push("run".to_string());
            if !prompt.is_empty() {
                args.push(prompt.to_string());
            }
        }

        match permissions {
            Permissions::AllowAll => {
                args.push("--auto".to_string());
            }
            Permissions::AcceptEdits | Permissions::Default => {}
        }

        if let Some(m) = model {
            args.push("--model".to_string());
            args.push(m.to_string());
        }

        AgentCommand {
            binary: "opencode".to_string(),
            args,
        }
    }

    // ── Aider ───────────────────────────────────────────────────────

    fn build_aider(
        prompt: &str,
        interactive: bool,
        _permissions: Permissions,
        model: Option<&str>,
    ) -> AgentCommand {
        let mut args = Vec::new();

        if !interactive {
            if !prompt.is_empty() {
                args.push("--message".to_string());
                args.push(prompt.to_string());
            }
            args.push("--yes".to_string());
            // Aider auto-commits by default; disable so the sandbox workspace
            // git history is not polluted with agent commits.
            args.push("--no-auto-commits".to_string());
        }

        if let Some(m) = model {
            args.push("--model".to_string());
            args.push(m.to_string());
        }

        AgentCommand {
            binary: "aider".to_string(),
            args,
        }
    }

    // ── Gemini CLI ──────────────────────────────────────────────────

    fn build_gemini(
        prompt: &str,
        interactive: bool,
        permissions: Permissions,
        model: Option<&str>,
    ) -> AgentCommand {
        let mut args = Vec::new();

        if !interactive {
            args.push("-p".to_string());
            if !prompt.is_empty() {
                args.push(prompt.to_string());
            }
        }

        if interactive {
            // Skip Gemini's folder-trust prompt; the workspace is the sandbox's
            // disposable clone, so there is nothing to confirm.
            args.push("--skip-trust".to_string());
        }

        match permissions {
            Permissions::AllowAll => {
                args.push("--approval-mode".to_string());
                args.push("yolo".to_string());
            }
            Permissions::AcceptEdits | Permissions::Default => {}
        }

        if let Some(m) = model {
            args.push("--model".to_string());
            args.push(m.to_string());
        }

        AgentCommand {
            binary: "gemini".to_string(),
            args,
        }
    }

    // ── Crush ───────────────────────────────────────────────────────

    fn build_crush(
        prompt: &str,
        interactive: bool,
        permissions: Permissions,
        model: Option<&str>,
    ) -> AgentCommand {
        let mut args = Vec::new();

        if !interactive {
            args.push("run".to_string());
            if !prompt.is_empty() {
                args.push(prompt.to_string());
            }
        } else {
            // `--yolo` is a root-command flag (interactive TUI only).
            match permissions {
                Permissions::AllowAll => {
                    args.push("--yolo".to_string());
                }
                Permissions::AcceptEdits | Permissions::Default => {}
            }
        }

        if let Some(m) = model {
            args.push("--model".to_string());
            args.push(m.to_string());
        }

        AgentCommand {
            binary: "crush".to_string(),
            args,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_command_default() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Claude,
            "",
            false,
            false,
            Permissions::Default,
            None,
        );
        assert_eq!(cmd.binary, "claude");
        assert!(cmd.args.contains(&"--verbose".to_string()));
        assert!(cmd.args.contains(&"--output-format".to_string()));
        assert!(!cmd
            .args
            .contains(&"--dangerously-skip-permissions".to_string()));
    }

    #[test]
    fn test_claude_command_allow_all() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Claude,
            "do something",
            false,
            false,
            Permissions::AllowAll,
            None,
        );
        assert!(cmd.args.contains(&"--print".to_string()));
        assert!(cmd.args.contains(&"do something".to_string()));
        assert!(cmd
            .args
            .contains(&"--dangerously-skip-permissions".to_string()));
    }

    #[test]
    fn test_claude_command_with_model() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Claude,
            "",
            false,
            false,
            Permissions::Default,
            Some("claude-sonnet-4-5-20250929"),
        );
        assert!(cmd.args.contains(&"--model".to_string()));
        assert!(cmd.args.contains(&"claude-sonnet-4-5-20250929".to_string()));
    }

    #[test]
    fn test_codex_command() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Codex,
            "write tests",
            false,
            false,
            Permissions::Default,
            None,
        );
        assert_eq!(cmd.binary, "codex");
        assert!(cmd.args.contains(&"exec".to_string()));
        assert!(cmd.args.contains(&"--skip-git-repo-check".to_string()));
        assert!(cmd.args.contains(&"write tests".to_string()));
    }

    #[test]
    fn test_codex_command_allow_all() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Codex,
            "",
            false,
            false,
            Permissions::AllowAll,
            None,
        );
        assert!(cmd
            .args
            .contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
    }

    #[test]
    fn test_goose_command_with_prompt() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Goose,
            "do it",
            false,
            false,
            Permissions::Default,
            None,
        );
        assert_eq!(cmd.binary, "goose");
        assert!(cmd.args.contains(&"run".to_string()));
        assert!(cmd.args.contains(&"--text".to_string()));
        assert!(cmd.args.contains(&"do it".to_string()));
    }

    #[test]
    fn test_goose_command_no_prompt() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Goose,
            "",
            false,
            false,
            Permissions::Default,
            None,
        );
        assert_eq!(cmd.binary, "goose");
        assert!(
            !cmd.args.contains(&"configure".to_string()),
            "headless goose with empty prompt must not run `goose configure`, got {:?}",
            cmd.args
        );
    }

    #[test]
    fn test_cursor_command() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Cursor,
            "refactor this",
            false,
            false,
            Permissions::Default,
            None,
        );
        assert_eq!(cmd.binary, "cursor-agent");
        assert!(cmd.args.contains(&"--message".to_string()));
        assert!(cmd.args.contains(&"refactor this".to_string()));
    }

    #[test]
    fn test_cursor_command_auto_mode() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Cursor,
            "",
            true,
            false,
            Permissions::AllowAll,
            None,
        );
        assert!(cmd.args.contains(&"--trust".to_string()));
        assert!(cmd.args.contains(&"--approve-mcps".to_string()));
    }

    #[test]
    fn test_build_env_goose_allow_all() {
        let mut base = HashMap::new();
        base.insert("ANTHROPIC_API_KEY".to_string(), "sk-...".to_string());

        let env =
            AgentCommandBuilder::build_env(&AgentType::Goose, false, Permissions::AllowAll, &base);
        assert_eq!(env.get("GOOSE_MODE").unwrap(), "auto");
        assert_eq!(env.get("ANTHROPIC_API_KEY").unwrap(), "sk-...");
    }

    #[test]
    fn test_build_env_goose_accept_edits() {
        let base = HashMap::new();
        let env = AgentCommandBuilder::build_env(
            &AgentType::Goose,
            false,
            Permissions::AcceptEdits,
            &base,
        );
        assert_eq!(env.get("GOOSE_MODE").unwrap(), "smart_approve");
    }

    #[test]
    fn test_build_env_goose_default() {
        let base = HashMap::new();
        let env =
            AgentCommandBuilder::build_env(&AgentType::Goose, false, Permissions::Default, &base);
        assert!(!env.contains_key("GOOSE_MODE"));
    }

    #[test]
    fn test_build_env_non_goose() {
        let base = HashMap::new();
        let env =
            AgentCommandBuilder::build_env(&AgentType::Claude, false, Permissions::AllowAll, &base);
        assert!(!env.contains_key("GOOSE_MODE"));
    }

    #[test]
    fn test_build_env_cursor_disables_debug_log() {
        let base = HashMap::new();
        let env =
            AgentCommandBuilder::build_env(&AgentType::Cursor, false, Permissions::Default, &base);
        assert_eq!(
            env.get("CURSOR_AGENT_DISABLE_DEBUG_LOG").map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn test_build_env_claude_sets_config_dir() {
        let base = HashMap::new();
        let env =
            AgentCommandBuilder::build_env(&AgentType::Claude, false, Permissions::Default, &base);
        assert_eq!(
            env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/home/developer/.claude")
        );
    }

    #[test]
    fn test_auto_mode_forces_allow_all() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Claude,
            "",
            true,
            false,
            Permissions::Default,
            None,
        );
        // auto_mode with Default permissions → effective AllowAll
        assert!(cmd
            .args
            .contains(&"--dangerously-skip-permissions".to_string()));
    }

    #[test]
    fn test_claude_interactive_omits_headless_flags() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Claude,
            "ignored prompt",
            false,
            true,
            Permissions::Default,
            None,
        );
        assert_eq!(cmd.binary, "claude");
        assert!(!cmd.args.contains(&"--print".to_string()));
        assert!(!cmd.args.contains(&"--verbose".to_string()));
        assert!(!cmd.args.contains(&"--output-format".to_string()));
        assert!(!cmd.args.contains(&"ignored prompt".to_string()));
    }

    #[test]
    fn test_claude_interactive_keeps_permission_and_model() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Claude,
            "",
            false,
            true,
            Permissions::AllowAll,
            Some("claude-sonnet-4-5-20250929"),
        );
        assert!(cmd
            .args
            .contains(&"--dangerously-skip-permissions".to_string()));
        assert!(cmd.args.contains(&"--model".to_string()));
        assert!(cmd.args.contains(&"claude-sonnet-4-5-20250929".to_string()));
        assert!(!cmd.args.contains(&"--print".to_string()));
    }

    #[test]
    fn test_codex_interactive_is_bare_codex() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Codex,
            "ignored",
            false,
            true,
            Permissions::Default,
            None,
        );
        assert_eq!(cmd.binary, "codex");
        assert!(
            cmd.args.is_empty(),
            "interactive codex must be bare `codex` (not `codex exec`), got {:?}",
            cmd.args
        );
    }

    #[test]
    fn test_codex_interactive_resume() {
        let cmd = AgentCommandBuilder::build_with_session(
            &AgentType::Codex,
            "ignored",
            false,
            true,
            Permissions::Default,
            None,
            Some("sess-1"),
        );
        assert_eq!(cmd.binary, "codex");
        assert_eq!(cmd.args, vec!["resume".to_string(), "sess-1".to_string()]);
    }

    #[test]
    fn test_codex_headless_resume() {
        let cmd = AgentCommandBuilder::build_with_session(
            &AgentType::Codex,
            "continue the task",
            false,
            false,
            Permissions::Default,
            None,
            Some("sess-1"),
        );
        assert_eq!(cmd.binary, "codex");
        assert!(cmd.args.contains(&"exec".to_string()));
        assert!(cmd.args.contains(&"resume".to_string()));
        assert!(cmd.args.contains(&"sess-1".to_string()));
        assert_eq!(cmd.args.last().unwrap(), "continue the task");

        let idx_exec = cmd.args.iter().position(|a| a == "exec").unwrap();
        let idx_skip = cmd
            .args
            .iter()
            .position(|a| a == "--skip-git-repo-check")
            .unwrap();
        let idx_json = cmd.args.iter().position(|a| a == "--json").unwrap();
        let idx_resume = cmd.args.iter().position(|a| a == "resume").unwrap();
        assert!(
            idx_resume > idx_exec && idx_resume > idx_skip && idx_resume > idx_json,
            "resume must come after exec-only flags, got {:?}",
            cmd.args
        );
    }

    #[test]
    fn test_codex_headless_resume_with_model_and_permissions() {
        let cmd = AgentCommandBuilder::build_with_session(
            &AgentType::Codex,
            "continue",
            false,
            false,
            Permissions::AllowAll,
            Some("gpt-5"),
            Some("sess-1"),
        );
        assert!(cmd
            .args
            .contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        assert!(cmd.args.contains(&"--model".to_string()));
        assert!(cmd.args.contains(&"gpt-5".to_string()));
        assert!(cmd.args.contains(&"resume".to_string()));
        assert!(cmd.args.contains(&"sess-1".to_string()));
        assert_eq!(cmd.args.last().unwrap(), "continue");
    }

    #[test]
    fn test_goose_interactive_uses_session() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Goose,
            "ignored",
            false,
            true,
            Permissions::Default,
            None,
        );
        assert_eq!(cmd.args, vec!["session".to_string()]);
    }

    #[test]
    fn test_cursor_interactive_omits_message() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Cursor,
            "ignored",
            false,
            true,
            Permissions::Default,
            None,
        );
        assert!(!cmd.args.contains(&"--message".to_string()));
        assert!(!cmd.args.contains(&"ignored".to_string()));
        assert!(
            cmd.args.contains(&"--trust".to_string()),
            "interactive cursor must pass --trust to skip the workspace-trust prompt, got {:?}",
            cmd.args
        );
    }

    #[test]
    fn test_auto_mode_suppresses_interactive() {
        // interactive is ignored when auto_mode is on (headless wins).
        let cmd = AgentCommandBuilder::build(
            &AgentType::Claude,
            "task",
            true,
            true,
            Permissions::Default,
            None,
        );
        assert!(cmd.args.contains(&"--print".to_string()));
        assert!(cmd.args.contains(&"--output-format".to_string()));
    }

    #[test]
    fn test_opencode_headless() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Opencode,
            "do it",
            false,
            false,
            Permissions::Default,
            None,
        );
        assert_eq!(cmd.binary, "opencode");
        assert_eq!(cmd.args, vec!["run".to_string(), "do it".to_string()]);
    }

    #[test]
    fn test_opencode_interactive_is_bare() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Opencode,
            "ignored",
            false,
            true,
            Permissions::Default,
            None,
        );
        assert_eq!(cmd.binary, "opencode");
        assert!(
            cmd.args.is_empty(),
            "interactive opencode must be bare `opencode`, got {:?}",
            cmd.args
        );
    }

    #[test]
    fn test_opencode_allow_all_adds_auto() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Opencode,
            "do it",
            false,
            false,
            Permissions::AllowAll,
            None,
        );
        assert!(cmd.args.contains(&"--auto".to_string()));
    }

    #[test]
    fn test_aider_headless() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Aider,
            "do it",
            false,
            false,
            Permissions::Default,
            None,
        );
        assert_eq!(cmd.binary, "aider");
        assert!(cmd.args.contains(&"--message".to_string()));
        assert!(cmd.args.contains(&"do it".to_string()));
        assert!(cmd.args.contains(&"--yes".to_string()));
        assert!(cmd.args.contains(&"--no-auto-commits".to_string()));
    }

    #[test]
    fn test_aider_interactive_is_bare() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Aider,
            "ignored",
            false,
            true,
            Permissions::Default,
            None,
        );
        assert_eq!(cmd.binary, "aider");
        assert!(
            cmd.args.is_empty(),
            "interactive aider must be bare `aider`, got {:?}",
            cmd.args
        );
    }

    #[test]
    fn test_gemini_headless() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Gemini,
            "do it",
            false,
            false,
            Permissions::Default,
            None,
        );
        assert_eq!(cmd.binary, "gemini");
        assert_eq!(cmd.args, vec!["-p".to_string(), "do it".to_string()]);
    }

    #[test]
    fn test_gemini_interactive_skips_trust() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Gemini,
            "ignored",
            false,
            true,
            Permissions::Default,
            None,
        );
        assert_eq!(cmd.binary, "gemini");
        assert_eq!(
            cmd.args,
            vec!["--skip-trust".to_string()],
            "interactive gemini must pass --skip-trust to skip the folder-trust prompt"
        );
    }

    #[test]
    fn test_gemini_allow_all_adds_yolo() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Gemini,
            "do it",
            false,
            false,
            Permissions::AllowAll,
            None,
        );
        assert!(cmd.args.contains(&"--approval-mode".to_string()));
        assert!(cmd.args.contains(&"yolo".to_string()));
    }

    #[test]
    fn test_crush_headless() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Crush,
            "do it",
            false,
            false,
            Permissions::Default,
            None,
        );
        assert_eq!(cmd.binary, "crush");
        assert_eq!(cmd.args, vec!["run".to_string(), "do it".to_string()]);
    }

    #[test]
    fn test_crush_interactive_is_bare() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Crush,
            "ignored",
            false,
            true,
            Permissions::Default,
            None,
        );
        assert_eq!(cmd.binary, "crush");
        assert!(
            cmd.args.is_empty(),
            "interactive crush must be bare `crush`, got {:?}",
            cmd.args
        );
    }

    #[test]
    fn test_crush_interactive_allow_all_adds_yolo() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Crush,
            "",
            false,
            true,
            Permissions::AllowAll,
            None,
        );
        assert!(cmd.args.contains(&"--yolo".to_string()));
    }

    #[test]
    fn test_all_agents_interactive_and_headless_argv() {
        let cases: &[(AgentType, &str, &[&str], &[&str])] = &[
            (AgentType::Claude, "claude", &[], &["--print", "do it"]),
            (
                AgentType::Codex,
                "codex",
                &[],
                &["exec", "--skip-git-repo-check", "do it"],
            ),
            (
                AgentType::Goose,
                "goose",
                &["session"],
                &["run", "--text", "do it"],
            ),
            (
                AgentType::Cursor,
                "cursor-agent",
                &["--trust"],
                &["--message", "do it"],
            ),
            (AgentType::Opencode, "opencode", &[], &["run", "do it"]),
            (
                AgentType::Aider,
                "aider",
                &[],
                &["--message", "do it", "--yes", "--no-auto-commits"],
            ),
            (AgentType::Gemini, "gemini", &["--skip-trust"], &["-p", "do it"]),
            (AgentType::Crush, "crush", &[], &["run", "do it"]),
        ];

        assert_eq!(
            cases.len(),
            AgentType::ALL.len(),
            "table must cover every agent in AgentType::ALL"
        );

        for (agent_type, binary, interactive_args, headless_args) in cases.iter().copied() {
            let interactive_cmd = AgentCommandBuilder::build(
                &agent_type,
                "ignored prompt",
                false,
                true,
                Permissions::Default,
                None,
            );
            assert_eq!(
                interactive_cmd.binary, binary,
                "interactive binary for {:?}",
                agent_type
            );
            assert_eq!(
                interactive_cmd.args,
                interactive_args
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>(),
                "interactive argv for {:?} must be the agent's TUI",
                agent_type,
            );

            let headless_cmd = AgentCommandBuilder::build(
                &agent_type,
                "do it",
                false,
                false,
                Permissions::Default,
                None,
            );
            assert_eq!(
                headless_cmd.binary, binary,
                "headless binary for {:?}",
                agent_type
            );
            for arg in headless_args {
                assert!(
                    headless_cmd.args.contains(&arg.to_string()),
                    "headless argv for {:?} should contain {:?}, got {:?}",
                    agent_type,
                    arg,
                    headless_cmd.args,
                );
            }
        }
    }
}
