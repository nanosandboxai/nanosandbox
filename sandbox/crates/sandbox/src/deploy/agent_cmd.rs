//! Agent command builder — ported from the legacy in-VM Go gateway (`buildAgentCommand`).
//!
//! Builds the CLI invocation (binary + args) for each agent type, and resolves
//! the environment map passed to the VM boot.

use std::collections::HashMap;
use crate::config::{AgentType, Permissions};

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
        let effective_permissions = permissions.effective(auto_mode);

        let interactive = interactive && !auto_mode;

        match agent_type {
            AgentType::Claude => {
                Self::build_claude(prompt, interactive, effective_permissions, model)
            }
            AgentType::Codex => {
                Self::build_codex(prompt, interactive, effective_permissions, model)
            }
            AgentType::Goose => {
                Self::build_goose(prompt, interactive, effective_permissions, model)
            }
            AgentType::Cursor => {
                Self::build_cursor(prompt, auto_mode, interactive, effective_permissions, model)
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
            AgentType::Goose => {
                match effective_permissions {
                    Permissions::AllowAll => {
                        env.insert("GOOSE_MODE".to_string(), "auto".to_string());
                    }
                    Permissions::AcceptEdits => {
                        env.insert("GOOSE_MODE".to_string(), "smart_approve".to_string());
                    }
                    Permissions::Default => {}
                }
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
    ) -> AgentCommand {
        let mut args = vec!["exec".to_string()];

        if !interactive {
            args.push("--skip-git-repo-check".to_string());
            match permissions {
                Permissions::AllowAll | Permissions::AcceptEdits => {
                    args.push("--dangerously-bypass-approvals-and-sandbox".to_string());
                }
                Permissions::Default => {}
            }

            args.push("--json".to_string());
        }

        if let Some(m) = model {
            args.push("--model".to_string());
            args.push(m.to_string());
        }

        if !interactive && !prompt.is_empty() {
            args.push(prompt.to_string());
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

        if !interactive && !prompt.is_empty() {
            args.push("run".to_string());
            args.push("--text".to_string());
            args.push(prompt.to_string());
        } else {
            // Interactive mode — goose configure if no provider configured
            args.push("configure".to_string());
        }

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
        assert!(!cmd.args.contains(&"--dangerously-skip-permissions".to_string()));
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
        assert!(cmd.args.contains(&"--dangerously-skip-permissions".to_string()));
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
        assert!(cmd.args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
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
        assert!(cmd.args.contains(&"configure".to_string()));
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

        let env = AgentCommandBuilder::build_env(
            &AgentType::Goose,
            false,
            Permissions::AllowAll,
            &base,
        );
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
        let env = AgentCommandBuilder::build_env(
            &AgentType::Goose,
            false,
            Permissions::Default,
            &base,
        );
        assert!(!env.contains_key("GOOSE_MODE"));
    }

    #[test]
    fn test_build_env_non_goose() {
        let base = HashMap::new();
        let env = AgentCommandBuilder::build_env(
            &AgentType::Claude,
            false,
            Permissions::AllowAll,
            &base,
        );
        assert!(!env.contains_key("GOOSE_MODE"));
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
        assert!(cmd.args.contains(&"--dangerously-skip-permissions".to_string()));
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
        assert!(cmd.args.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(cmd.args.contains(&"--model".to_string()));
        assert!(cmd.args.contains(&"claude-sonnet-4-5-20250929".to_string()));
        assert!(!cmd.args.contains(&"--print".to_string()));
    }

    #[test]
    fn test_codex_interactive_is_bare_exec() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Codex,
            "ignored",
            false,
            true,
            Permissions::Default,
            None,
        );
        assert_eq!(cmd.args, vec!["exec".to_string()]);
    }

    #[test]
    fn test_goose_interactive_uses_configure() {
        let cmd = AgentCommandBuilder::build(
            &AgentType::Goose,
            "ignored",
            false,
            true,
            Permissions::Default,
            None,
        );
        assert!(cmd.args.contains(&"configure".to_string()));
        assert!(!cmd.args.contains(&"run".to_string()));
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
}
