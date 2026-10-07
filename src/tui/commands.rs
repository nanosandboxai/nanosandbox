//! Slash command parsing and autocomplete.

/// Parsed slash command.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Exit the TUI.
    Quit,
    /// Show help text.
    Help,
    /// Close (hide) a panel. Sandbox keeps running.
    Close {
        /// Target: panel index or name, or None for focused panel.
        target: Option<String>,
    },
    /// Show a previously hidden panel.
    Open {
        /// Target: panel index or name, or None for last-hidden panel.
        target: Option<String>,
    },
    /// Add a new agent panel, optionally with a custom image.
    AddAgent {
        /// Agent name.
        agent: String,
        /// Optional custom container image.
        image: Option<String>,
        /// Optional image tag (e.g., "rc11", "v1.0"). Defaults to "latest".
        tag: Option<String>,
        /// Optional project path to mount.
        project: Option<String>,
        /// Optional branch name for the project clone.
        branch: Option<String>,
        /// Optional sandbox name.
        name: Option<String>,
        /// Run in headless/autonomous mode.
        auto_mode: bool,
        /// Run the agent interactively on a TTY console.
        interactive: bool,
        /// Task prompt for headless mode (required with --auto-mode).
        prompt: Option<String>,
        /// Optional model identifier (e.g., "claude-sonnet-4-5-20250929").
        model: Option<String>,
        /// Runtime env keys to import from nanosb startup env pool.
        use_env: Vec<String>,
        /// Optional env file path to merge into this panel's runtime env.
        env_file: Option<String>,
        /// Run agent commands as root inside the sandbox VM.
        run_as_root: bool,
    },
    /// Switch focus to a specific panel index.
    Focus {
        /// Zero-based panel index.
        panel: usize,
    },
    /// Toggle the MCP sidebar.
    McpToggle,
    /// List configured MCP servers.
    McpList,
    /// Set or list environment variables for the focused panel.
    Env {
        /// KEY=VALUE pair to set, or None to list current env vars.
        assignment: Option<(String, String)>,
    },
    /// Kill (destroy) a sandbox and remove its panel.
    Kill {
        /// Panel target: index or name, or None to kill focused panel.
        panel: Option<String>,
    },
    /// Reconnect SSH terminal for the focused panel.
    Reconnect,
    /// Toggle the sandbox sidebar.
    Sandboxes,
    /// Copy focused panel content to system clipboard.
    Copy,
    /// Toggle zoom (maximize/minimize) for the focused panel.
    Zoom,
    /// List git branches created by nanosb sandboxes.
    Branches,
    /// Git sync control: show status, enable, disable, or manual sync.
    GitSync {
        /// Subcommand: None (status), "on", "off", "now"
        action: Option<String>,
    },
    /// Open clone directory in an external tool.
    Edit {
        /// Tool override, or None for preferred/auto-detected.
        tool: Option<String>,
    },
    /// Switch or list TUI colour themes.
    Theme {
        /// Theme name to switch to, or None to list available themes.
        name: Option<String>,
    },
    /// Toggle the skills sidebar / list skills.
    SkillsList,
    /// Show details of a skill.
    SkillsShow {
        /// Skill name.
        name: String,
    },
    /// Show current agent definition.
    AgentShow,
    /// List available agents in the registry.
    AgentList,
    /// Show details of a registry agent.
    AgentInfo {
        /// Agent name.
        name: String,
    },
    /// Upload a file from the host into the sandbox VM.
    Upload {
        /// Host file path.
        path: String,
    },
    /// Paste an image from the system clipboard into the sandbox VM.
    PasteImage,
    /// Destroy all sandboxes, remove session state, and exit.
    Destroy,
    /// Clear the command history.
    ClearHistory,
}

/// Result of parsing a slash command.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseResult {
    /// Successfully parsed command.
    Ok(Command),
    /// The input is not a slash command (regular message).
    NotACommand,
    /// The input is a slash command but has errors; contains a help message.
    Err(String),
}

/// Supported agent names for `/add`.
const SUPPORTED_AGENTS: &[&str] = &["claude", "goose", "codex", "cursor"];

/// A single line of `/help` output: a command pattern and its description.
///
/// This is the **single source of truth** for the advertised command surface.
/// `format_help()` renders it and `HELP_ENTRIES` is asserted against the parser
/// so no advertised command can be a dead stub.
pub struct CommandHelpEntry {
    /// Usage pattern, e.g. `"/focus <n>"`, or empty for a free-form note.
    pub pattern: &'static str,
    /// Human-readable description shown to the right of the pattern.
    pub description: &'static str,
}

/// The advertised command surface, rendered by [`format_help`].
///
/// Ordering is intentional: core panel lifecycle first, then environment,
/// registry introspection, git, and finally session control.
pub static HELP_ENTRIES: &[CommandHelpEntry] = &[
    CommandHelpEntry {
        pattern: "/add <agent> [--tag <version>] [--model <model>] [--interactive] [--auto-mode -p <prompt>] [--run-as-root] [--image <img>] [--project <path>] [--branch <name>] [--name <name>] [--env-file <path>] [--use-env <KEY>]...",
        description: "Add a new agent panel",
    },
    CommandHelpEntry {
        pattern: "/sandboxes",
        description: "Toggle sandbox sidebar",
    },
    CommandHelpEntry {
        pattern: "/focus <n>",
        description: "Focus panel n (0-indexed)",
    },
    CommandHelpEntry {
        pattern: "/close [n|name]",
        description: "Hide panel (sandbox keeps running)",
    },
    CommandHelpEntry {
        pattern: "/open [n|name]",
        description: "Show a hidden panel",
    },
    CommandHelpEntry {
        pattern: "/kill [n|name]",
        description: "Kill sandbox & remove panel",
    },
    CommandHelpEntry {
        pattern: "/copy",
        description: "Copy panel content to clipboard",
    },
    CommandHelpEntry {
        pattern: "/upload <path>",
        description: "Upload host file to sandbox",
    },
    CommandHelpEntry {
        pattern: "/paste-image",
        description: "Paste clipboard image to sandbox",
    },
    CommandHelpEntry {
        pattern: "/zoom",
        description: "Toggle panel zoom (Ctrl+F)",
    },
    CommandHelpEntry {
        pattern: "/theme [name]",
        description: "Switch colour theme",
    },
    CommandHelpEntry {
        pattern: "/env [KEY=VALUE]",
        description: "Set/list panel env vars",
    },
    CommandHelpEntry {
        pattern: "/reconnect",
        description: "Reconnect SSH terminal",
    },
    CommandHelpEntry {
        pattern: "/branches",
        description: "List nanosb branches in project",
    },
    CommandHelpEntry {
        pattern: "/gitsync [on|off|now]",
        description: "Sync sandbox commits to local repo",
    },
    CommandHelpEntry {
        pattern: "/mcp",
        description: "Toggle MCP sidebar",
    },
    CommandHelpEntry {
        pattern: "/mcp list",
        description: "List MCP servers (from sandbox.yml)",
    },
    CommandHelpEntry {
        pattern: "/skills [list]",
        description: "List skills (from sandbox.yml)",
    },
    CommandHelpEntry {
        pattern: "/skills show <name>",
        description: "Show skill details",
    },
    CommandHelpEntry {
        pattern: "/agent list",
        description: "List available agents",
    },
    CommandHelpEntry {
        pattern: "/agent show <name>",
        description: "Show agent details",
    },
    CommandHelpEntry {
        pattern: "/edit [tool]",
        description: "Open clone in external tool",
    },
    CommandHelpEntry {
        pattern: "/clearhistory",
        description: "Clear command history",
    },
    CommandHelpEntry {
        pattern: "/quit",
        description: "Suspend session and exit",
    },
    CommandHelpEntry {
        pattern: "/destroy",
        description: "Full cleanup and exit",
    },
    CommandHelpEntry {
        pattern: "",
        description: "Config is declarative: edit sandbox.yml and run `nanosb apply`.",
    },
];

/// Render the `/help` overlay text from [`HELP_ENTRIES`].
///
/// Kept in sync with the parser by `test_help_entries_are_all_recognized_commands`.
pub fn format_help() -> String {
    let mut lines = vec!["Available commands:".to_string()];
    for entry in HELP_ENTRIES {
        if entry.pattern.is_empty() {
            lines.push(String::new());
            lines.push(format!("  {}", entry.description));
        } else {
            lines.push(format!("  {:<74}{}", entry.pattern, entry.description));
        }
    }
    lines.push(String::new());
    lines.push("  Press Esc to dismiss.".to_string());
    lines.join("\n")
}

const ALL_COMMANDS: &[&str] = &[
    "/quit", "/q", "/destroy", "/help", "/clearhistory", "/close", "/copy",
    "/add", "/focus", "/kill", "/reconnect", "/env",
    "/zoom", "/branches",
    "/gitsync", "/gitsync on", "/gitsync off", "/gitsync now",
    "/open", "/edit",
    "/sandboxes",
    "/theme", "/theme nanosandbox", "/theme nanosandbox-light",
    "/theme dracula", "/theme catppuccin", "/theme tokyo-night", "/theme nord",
    "/mcp", "/mcp list",
    "/skills", "/skills list", "/skills show",
    "/agent list", "/agent show",
    "/upload", "/paste-image",
];

/// Parse a line of input into a Command, or None if it's a regular message.
///
/// For backward compatibility, returns `Option<Command>`. Use [`parse_command_verbose`]
/// to get detailed error messages.
pub fn parse_command(input: &str) -> Option<Command> {
    match parse_command_verbose(input) {
        ParseResult::Ok(cmd) => Some(cmd),
        _ => None,
    }
}

/// Parse a line of input with detailed error messages for invalid commands.
pub fn parse_command_verbose(input: &str) -> ParseResult {
    let input = input.trim();
    if !input.starts_with('/') {
        return ParseResult::NotACommand;
    }

    let parts: Vec<&str> = input.split_whitespace().collect();
    if parts.is_empty() {
        return ParseResult::NotACommand;
    }

    match parts[0] {
        "/quit" | "/q" => ParseResult::Ok(Command::Quit),
        "/destroy" => ParseResult::Ok(Command::Destroy),
        "/help" => ParseResult::Ok(Command::Help),
        "/clearhistory" => ParseResult::Ok(Command::ClearHistory),
        "/close" => {
            let target = parts.get(1).map(|s| s.to_string());
            ParseResult::Ok(Command::Close { target })
        }

        "/add" => parse_add(&parts),
        "/focus" => parse_focus(&parts),
        "/mcp" => parse_mcp(&parts),
        "/env" => parse_env(&parts),
        "/kill" => parse_kill(&parts),
        "/sandboxes" => ParseResult::Ok(Command::Sandboxes),
        "/reconnect" => ParseResult::Ok(Command::Reconnect),
        "/copy" => ParseResult::Ok(Command::Copy),
        "/zoom" => ParseResult::Ok(Command::Zoom),
        "/branches" => ParseResult::Ok(Command::Branches),
        "/gitsync" => parse_gitsync(&parts),
        "/open" => {
            let target = parts.get(1).map(|s| s.to_string());
            ParseResult::Ok(Command::Open { target })
        }
        "/edit" => {
            let tool = parts.get(1).map(|s| s.to_string());
            ParseResult::Ok(Command::Edit { tool })
        }
        "/theme" => parse_theme(&parts),
        "/skills" => parse_skills(&parts),
        "/agent" => parse_agent(&parts),
        "/upload" => parse_upload(&parts),
        "/paste-image" => ParseResult::Ok(Command::PasteImage),

        other => ParseResult::Err(format!(
            "Unknown command: {}\nType /help for available commands.",
            other,
        )),
    }
}

fn parse_add(parts: &[&str]) -> ParseResult {
    let agent = match parts.get(1) {
        Some(a) => *a,
        None => {
            return ParseResult::Err(format!(
                "Usage: /add <agent> [--tag <version>] [--model <model>] [--interactive] [--auto-mode -p <prompt>] [--run-as-root] [--image <image>] [--project <path>] [--branch <name>] [--name <name>] [--env-file <path>] [--use-env <KEY>]...\n\
                 Supported agents: {}\n\
                 Example: /add claude\n\
                 With tag: /add claude --tag rc11\n\
                 With model: /add claude --model claude-sonnet-4-5-20250929\n\
                 Headless: /add claude --auto-mode -p \"list files\"\n\
                 With env file: /add claude --env-file .env.local\n\
                 With runtime env: /add claude --use-env OPENAI_API_KEY",
                SUPPORTED_AGENTS.join(", "),
            ));
        }
    };

    let mut image = None;
    let mut tag = None;
    let mut project = None;
    let mut branch = None;
    let mut name = None;
    let mut auto_mode = false;
    let mut interactive = false;
    let mut prompt = None;
    let mut model = None;
    let mut use_env: Vec<String> = Vec::new();
    let mut env_file = None;
    let mut run_as_root = false;
    let mut i = 2;

    while i < parts.len() {
        match parts[i] {
            "--image" => {
                match parts.get(i + 1) {
                    Some(v) => { image = Some(v.to_string()); i += 2; }
                    None => return ParseResult::Err(
                        "Usage: /add <agent> --image <image>\n\
                         Example: /add myagent --image ghcr.io/org/agent:latest".to_string(),
                    ),
                }
            }
            "--project" => {
                match parts.get(i + 1) {
                    Some(v) => {
                        let raw = std::path::Path::new(v);
                        let resolved = if raw.is_absolute() {
                            raw.to_path_buf()
                        } else {
                            std::env::current_dir()
                                .unwrap_or_default()
                                .join(raw)
                        };
                        if !resolved.exists() {
                            return ParseResult::Err(format!(
                                "Project path does not exist: {}\n\
                                 Usage: /add <agent> --project <path>",
                                resolved.display(),
                            ));
                        }
                        if !resolved.is_dir() {
                            return ParseResult::Err(format!(
                                "Project path is not a directory: {}\n\
                                 Usage: /add <agent> --project <path>",
                                resolved.display(),
                            ));
                        }
                        project = Some(resolved.to_string_lossy().to_string());
                        i += 2;
                    }
                    None => return ParseResult::Err(
                        "--project requires a path\n\
                         Usage: /add <agent> --project <path>".to_string(),
                    ),
                }
            }
            "--branch" => {
                match parts.get(i + 1) {
                    Some(v) => { branch = Some(v.to_string()); i += 2; }
                    None => return ParseResult::Err(
                        "--branch requires a name\n\
                         Usage: /add <agent> --branch <name>".to_string(),
                    ),
                }
            }
            "--name" => {
                match parts.get(i + 1) {
                    Some(v) => { name = Some(v.to_string()); i += 2; }
                    None => return ParseResult::Err(
                        "--name requires a value\n\
                         Usage: /add <agent> --name <name>".to_string(),
                    ),
                }
            }
            "--tag" => {
                match parts.get(i + 1) {
                    Some(v) => { tag = Some(v.to_string()); i += 2; }
                    None => return ParseResult::Err(
                        "--tag requires a value\n\
                         Usage: /add <agent> --tag <version>\n\
                         Example: /add claude --tag rc11".to_string(),
                    ),
                }
            }
            "--model" => {
                match parts.get(i + 1) {
                    Some(v) => { model = Some(v.to_string()); i += 2; }
                    None => return ParseResult::Err(
                        "--model requires a value\n\
                         Usage: /add <agent> --model <model-name>\n\
                         Example: /add claude --model claude-sonnet-4-5-20250929".to_string(),
                    ),
                }
            }
            "--use-env" => {
                match parts.get(i + 1) {
                    Some(v) => {
                        use_env.push(v.to_string());
                        i += 2;
                    }
                    None => {
                        return ParseResult::Err(
                            "--use-env requires a key name\n\
                             Usage: /add <agent> --use-env <KEY>"
                                .to_string(),
                        )
                    }
                }
            }
            "--env-file" => {
                match parts.get(i + 1) {
                    Some(v) => {
                        env_file = Some(v.to_string());
                        i += 2;
                    }
                    None => {
                        return ParseResult::Err(
                            "--env-file requires a path\n\
                             Usage: /add <agent> --env-file <path>"
                                .to_string(),
                        )
                    }
                }
            }
            "--auto-mode" => {
                auto_mode = true;
                i += 1;
            }
            "--interactive" => {
                interactive = true;
                i += 1;
            }
            "--run-as-root" => {
                run_as_root = true;
                i += 1;
            }
            "-p" | "--prompt" => {
                // Consume all remaining tokens as the prompt text.
                let remaining: Vec<&str> = parts[i + 1..].to_vec();
                if remaining.is_empty() {
                    return ParseResult::Err(
                        "--prompt requires a value\n\
                         Usage: /add <agent> --auto-mode -p your task here".to_string(),
                    );
                }
                let joined = remaining.join(" ");
                // Strip surrounding quotes if present (users often type: -p "my task").
                let trimmed = joined.strip_prefix('"').unwrap_or(&joined);
                let trimmed = trimmed.strip_suffix('"').unwrap_or(trimmed);
                prompt = Some(trimmed.to_string());
                break; // -p consumes the rest of the input
            }
            other => {
                return ParseResult::Err(format!(
                    "Unknown option: {}\n\
                     Usage: /add <agent> [--tag <version>] [--model <model>] [--interactive] [--auto-mode -p <prompt>] [--run-as-root] [--image <image>] [--project <path>] [--branch <name>] [--name <name>] [--env-file <path>] [--use-env <KEY>]...",
                    other,
                ));
            }
        }
    }

    // Validate: prompt is required when auto-mode is enabled.
    if auto_mode && prompt.is_none() {
        return ParseResult::Err(
            "--prompt is required with --auto-mode\n\
             Usage: /add <agent> --auto-mode -p \"your task\"".to_string(),
        );
    }

    // Validate: interactive and auto-mode are mutually exclusive.
    if interactive && auto_mode {
        return ParseResult::Err(
            "--interactive and --auto-mode are mutually exclusive\n\
             Interactive runs the agent TUI; auto-mode is headless.".to_string(),
        );
    }

    if image.is_none() && !SUPPORTED_AGENTS.contains(&agent) {
        return ParseResult::Err(format!(
            "Unknown agent: '{}'\n\
             Supported agents: {}\n\
             Or use a custom image: /add {} --image <image>",
            agent,
            SUPPORTED_AGENTS.join(", "),
            agent,
        ));
    }

    ParseResult::Ok(Command::AddAgent {
        agent: agent.to_string(),
        image,
        tag,
        project,
        branch,
        name,
        auto_mode,
        interactive,
        prompt,
        model,
        use_env,
        env_file,
        run_as_root,
    })
}

fn parse_focus(parts: &[&str]) -> ParseResult {
    match parts.get(1) {
        Some(n) => match n.parse::<usize>() {
            Ok(panel) => ParseResult::Ok(Command::Focus { panel }),
            Err(_) => ParseResult::Err(format!(
                "'{}' is not a valid panel number.\n\
                 Usage: /focus <n>  (0-indexed panel number)\n\
                 Example: /focus 0",
                n,
            )),
        },
        None => ParseResult::Err(
            "Usage: /focus <n>  (0-indexed panel number)\n\
             Example: /focus 0"
                .to_string(),
        ),
    }
}

fn parse_mcp(parts: &[&str]) -> ParseResult {
    match parts.get(1).copied() {
        None => ParseResult::Ok(Command::McpToggle),
        Some("list") => ParseResult::Ok(Command::McpList),
        Some(sub) => ParseResult::Err(format!(
            "Unknown MCP subcommand: '{}'\n\
             Available: /mcp list\n\
             Config is declarative — edit sandbox.yml and redeploy (nanosb apply).",
            sub,
        )),
    }
}

fn parse_env(parts: &[&str]) -> ParseResult {
    if parts.len() == 1 {
        return ParseResult::Ok(Command::Env { assignment: None });
    }

    let rest = parts[1..].join(" ");
    if let Some(eq_pos) = rest.find('=') {
        let key = rest[..eq_pos].trim().to_string();
        let value = rest[eq_pos + 1..].trim().to_string();
        if key.is_empty() {
            return ParseResult::Err(
                "Usage: /env KEY=VALUE\n\
                 Example: /env ANTHROPIC_API_KEY=sk-ant-..."
                    .to_string(),
            );
        }
        ParseResult::Ok(Command::Env {
            assignment: Some((key, value)),
        })
    } else {
        ParseResult::Err(
            "Usage: /env KEY=VALUE  (set a variable)\n\
             Usage: /env            (list current variables)\n\
             Example: /env ANTHROPIC_API_KEY=sk-ant-..."
                .to_string(),
        )
    }
}

fn parse_kill(parts: &[&str]) -> ParseResult {
    match parts.get(1) {
        Some(n) => ParseResult::Ok(Command::Kill { panel: Some(n.to_string()) }),
        None => ParseResult::Ok(Command::Kill { panel: None }),
    }
}

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
             - /gitsync     Show current sync status\n\
             - /gitsync on  Auto-sync sandbox commits to your local repo branches\n\
             - /gitsync off Stop syncing (changes stay in sandbox clone only)\n\
             - /gitsync now Sync sandbox commits to local repo once",
            other,
        )),
    }
}

fn parse_theme(parts: &[&str]) -> ParseResult {
    match parts.get(1) {
        None => ParseResult::Ok(Command::Theme { name: None }),
        Some(name) => {
            use super::theme::ThemeName;
            match name.parse::<ThemeName>() {
                Ok(_) => ParseResult::Ok(Command::Theme {
                    name: Some(name.to_string()),
                }),
                Err(msg) => ParseResult::Err(msg),
            }
        }
    }
}

fn parse_skills(parts: &[&str]) -> ParseResult {
    match parts.get(1).copied() {
        None | Some("list") => ParseResult::Ok(Command::SkillsList),
        Some("show") => match parts.get(2) {
            Some(name) => ParseResult::Ok(Command::SkillsShow {
                name: name.to_string(),
            }),
            None => ParseResult::Err(
                "Usage: /skills show <name>\n\
                 Use /skills list to see active skills."
                    .to_string(),
            ),
        },
        Some(sub) => ParseResult::Err(format!(
            "Unknown skills subcommand: '{}'\n\
             Available: /skills [list], /skills show\n\
             Config is declarative — edit sandbox.yml and redeploy (nanosb apply).",
            sub,
        )),
    }
}

fn parse_agent(parts: &[&str]) -> ParseResult {
    match parts.get(1).copied() {
        None => ParseResult::Ok(Command::AgentShow),
        Some("list") => ParseResult::Ok(Command::AgentList),
        Some("show") => match parts.get(2) {
            Some(name) => ParseResult::Ok(Command::AgentInfo {
                name: name.to_string(),
            }),
            None => ParseResult::Err(
                "Usage: /agent show <name>\n\
                 Use /agent list to see available agents."
                    .to_string(),
            ),
        },
        Some(sub) => ParseResult::Err(format!(
            "Unknown agent subcommand: '{}'\n\
             Available: /agent, /agent list, /agent show\n\
             Config is declarative — edit sandbox.yml and redeploy (nanosb apply).",
            sub,
        )),
    }
}

fn parse_upload(parts: &[&str]) -> ParseResult {
    match parts.get(1) {
        Some(_) => {
            // Rejoin in case the path was split by whitespace (unlikely for absolute paths).
            let path = parts[1..].join(" ");
            ParseResult::Ok(Command::Upload { path })
        }
        None => ParseResult::Err(
            "Usage: /upload <host-path>\n\
             Uploads a file from the host into the sandbox at /workspace/.uploads/\n\
             Example: /upload /Users/me/screenshot.png"
                .to_string(),
        ),
    }
}

/// Return autocomplete suggestions for a partial input.
pub fn autocomplete(partial: &str) -> Vec<String> {
    ALL_COMMANDS
        .iter()
        .filter(|cmd| cmd.starts_with(partial))
        .map(|cmd| cmd.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_command() {
        assert_eq!(parse_command("/quit"), Some(Command::Quit));
    }

    #[test]
    fn test_parse_q_alias() {
        assert_eq!(parse_command("/q"), Some(Command::Quit));
    }

    #[test]
    fn test_parse_add_agent() {
        assert_eq!(
            parse_command("/add claude"),
            Some(Command::AddAgent {
                agent: "claude".to_string(),
                image: None,
                tag: None,
                project: None,
                branch: None,
                name: None,
                auto_mode: false,
                interactive: false,
                prompt: None,
                model: None,
                use_env: vec![],
                env_file: None,
                run_as_root: false,
            })
        );
    }

    #[test]
    fn test_parse_add_agent_with_image() {
        assert_eq!(
            parse_command("/add claude --image my-registry/claude:v2"),
            Some(Command::AddAgent {
                agent: "claude".to_string(),
                image: Some("my-registry/claude:v2".to_string()),
                tag: None,
                project: None,
                branch: None,
                name: None,
                auto_mode: false,
                interactive: false,
                prompt: None,
                model: None,
                use_env: vec![],
                env_file: None,
                run_as_root: false,
            })
        );
    }

    #[test]
    fn test_parse_add_agent_with_run_as_root() {
        assert_eq!(
            parse_command("/add claude --run-as-root"),
            Some(Command::AddAgent {
                agent: "claude".to_string(),
                image: None,
                tag: None,
                project: None,
                branch: None,
                name: None,
                auto_mode: false,
                interactive: false,
                prompt: None,
                model: None,
                use_env: vec![],
                env_file: None,
                run_as_root: true,
            })
        );
    }

    #[test]
    fn test_parse_not_a_command() {
        assert_eq!(parse_command("hello world"), None);
    }

    #[test]
    fn test_parse_mcp_toggle() {
        assert_eq!(parse_command("/mcp"), Some(Command::McpToggle));
    }

    #[test]
    fn test_removed_mcp_subcommands_are_not_commands() {
        for sub in ["add", "remove", "enable", "disable"] {
            let input = format!("/mcp {} github", sub);
            let result = parse_command_verbose(&input);
            match result {
                ParseResult::Err(msg) => {
                    assert!(
                        msg.contains("Unknown MCP subcommand"),
                        "{} should be rejected as an unknown subcommand, got: {}",
                        sub,
                        msg
                    );
                }
                other => panic!("expected Err for {}, got {:?}", sub, other),
            }
        }
    }

    #[test]
    fn test_parse_focus() {
        assert_eq!(parse_command("/focus 2"), Some(Command::Focus { panel: 2 }));
    }

    #[test]
    fn test_autocomplete_slash() {
        let suggestions = autocomplete("/");
        assert!(suggestions.len() > 5);
    }

    #[test]
    fn test_autocomplete_partial() {
        let suggestions = autocomplete("/mc");
        assert!(suggestions.iter().any(|s| s.starts_with("/mcp")));
        assert!(!suggestions.iter().any(|s| s.starts_with("/quit")));
    }

    // ===== New error message tests =====

    #[test]
    fn test_add_missing_agent_shows_help() {
        let result = parse_command_verbose("/add");
        match result {
            ParseResult::Err(msg) => {
                assert!(msg.contains("Usage:"), "should show usage");
                assert!(msg.contains("claude"), "should list supported agents");
            }
            other => panic!("expected Err, got {:?}", other),
        }
    }

    #[test]
    fn test_add_unsupported_agent_shows_error() {
        let result = parse_command_verbose("/add foobar");
        match result {
            ParseResult::Err(msg) => {
                assert!(msg.contains("Unknown agent"));
                assert!(msg.contains("foobar"));
                assert!(msg.contains("claude"), "should list supported agents");
                assert!(msg.contains("--image"), "should suggest custom image");
            }
            other => panic!("expected Err, got {:?}", other),
        }
    }

    #[test]
    fn test_add_custom_image_bypasses_agent_validation() {
        assert_eq!(
            parse_command_verbose("/add myagent --image foo/bar:latest"),
            ParseResult::Ok(Command::AddAgent {
                agent: "myagent".to_string(),
                image: Some("foo/bar:latest".to_string()),
                tag: None,
                project: None,
                branch: None,
                name: None,
                auto_mode: false,
                interactive: false,
                prompt: None,
                model: None,
                use_env: vec![],
                env_file: None,
                run_as_root: false,
            })
        );
    }

    #[test]
    fn test_add_image_flag_missing_value() {
        let result = parse_command_verbose("/add claude --image");
        match result {
            ParseResult::Err(msg) => assert!(msg.contains("Usage:")),
            other => panic!("expected Err, got {:?}", other),
        }
    }

    #[test]
    fn test_focus_missing_number_shows_help() {
        let result = parse_command_verbose("/focus");
        match result {
            ParseResult::Err(msg) => assert!(msg.contains("Usage:")),
            other => panic!("expected Err, got {:?}", other),
        }
    }

    #[test]
    fn test_focus_invalid_number_shows_error() {
        let result = parse_command_verbose("/focus abc");
        match result {
            ParseResult::Err(msg) => {
                assert!(msg.contains("abc"));
                assert!(msg.contains("not a valid panel number"));
            }
            other => panic!("expected Err, got {:?}", other),
        }
    }

    #[test]
    fn test_mcp_unknown_subcommand() {
        let result = parse_command_verbose("/mcp foo");
        match result {
            ParseResult::Err(msg) => {
                assert!(msg.contains("Unknown MCP subcommand"));
                assert!(msg.contains("foo"));
            }
            other => panic!("expected Err, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_sandboxes() {
        assert_eq!(parse_command("/sandboxes"), Some(Command::Sandboxes));
    }

    #[test]
    fn test_unknown_command() {
        let result = parse_command_verbose("/foobar");
        match result {
            ParseResult::Err(msg) => {
                assert!(msg.contains("Unknown command"));
                assert!(msg.contains("/foobar"));
            }
            other => panic!("expected Err, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_env_no_args() {
        assert_eq!(
            parse_command_verbose("/env"),
            ParseResult::Ok(Command::Env { assignment: None }),
        );
    }

    #[test]
    fn test_parse_env_with_assignment() {
        assert_eq!(
            parse_command_verbose("/env MY_KEY=my_value"),
            ParseResult::Ok(Command::Env {
                assignment: Some(("MY_KEY".to_string(), "my_value".to_string())),
            }),
        );
    }

    #[test]
    fn test_parse_env_missing_equals() {
        let result = parse_command_verbose("/env JUST_A_KEY");
        assert!(matches!(result, ParseResult::Err(_)));
    }

    #[test]
    fn test_parse_kill_no_arg() {
        assert_eq!(
            parse_command_verbose("/kill"),
            ParseResult::Ok(Command::Kill { panel: None }),
        );
    }

    #[test]
    fn test_parse_kill_with_number() {
        assert_eq!(
            parse_command_verbose("/kill 2"),
            ParseResult::Ok(Command::Kill { panel: Some("2".to_string()) }),
        );
    }

    #[test]
    fn test_parse_kill_with_name() {
        assert_eq!(
            parse_command_verbose("/kill claude"),
            ParseResult::Ok(Command::Kill { panel: Some("claude".to_string()) }),
        );
    }

    #[test]
    fn test_parse_zoom() {
        assert_eq!(parse_command("/zoom"), Some(Command::Zoom));
    }

    #[test]
    fn test_parse_max_is_unknown() {
        let result = parse_command_verbose("/max");
        assert!(matches!(result, ParseResult::Err(_)));
    }

    #[test]
    fn test_parse_add_with_project() {
        // Use /tmp which always exists on macOS/Linux.
        assert_eq!(
            parse_command_verbose("/add claude --project /tmp"),
            ParseResult::Ok(Command::AddAgent {
                agent: "claude".to_string(),
                image: None,
                tag: None,
                project: Some("/tmp".to_string()),
                branch: None,
                name: None,
                auto_mode: false,
                interactive: false,
                prompt: None,
                model: None,
                use_env: vec![],
                env_file: None,
                run_as_root: false,
            })
        );
    }

    #[test]
    fn test_parse_add_with_project_and_branch() {
        assert_eq!(
            parse_command_verbose("/add claude --project /tmp --branch feat/auth"),
            ParseResult::Ok(Command::AddAgent {
                agent: "claude".to_string(),
                image: None,
                tag: None,
                project: Some("/tmp".to_string()),
                branch: Some("feat/auth".to_string()),
                name: None,
                auto_mode: false,
                interactive: false,
                prompt: None,
                model: None,
                use_env: vec![],
                env_file: None,
                run_as_root: false,
            })
        );
    }

    #[test]
    fn test_parse_add_project_path_not_exists() {
        let result = parse_command_verbose("/add claude --project /nonexistent/path/xyz");
        assert!(matches!(result, ParseResult::Err(msg) if msg.contains("does not exist")));
    }

    #[test]
    fn test_parse_add_project_path_not_directory() {
        // /etc/hosts is a file, not a directory.
        let result = parse_command_verbose("/add claude --project /etc/hosts");
        assert!(matches!(result, ParseResult::Err(msg) if msg.contains("not a directory")));
    }

    #[test]
    fn test_parse_add_project_missing_value() {
        let result = parse_command_verbose("/add claude --project");
        assert!(matches!(result, ParseResult::Err(msg) if msg.contains("--project requires a path")));
    }

    #[test]
    fn test_parse_add_auto_mode_with_prompt() {
        assert_eq!(
            parse_command_verbose("/add claude --auto-mode -p list files"),
            ParseResult::Ok(Command::AddAgent {
                agent: "claude".to_string(),
                image: None,
                tag: None,
                project: None,
                branch: None,
                name: None,
                auto_mode: true,
                interactive: false,
                prompt: Some("list files".to_string()),
                model: None,
                use_env: vec![],
                env_file: None,
                run_as_root: false,
            })
        );
    }

    #[test]
    fn test_parse_add_auto_mode_prompt_with_quotes() {
        // Users often type: -p "analyse project" — quotes should be stripped.
        assert_eq!(
            parse_command_verbose("/add claude --auto-mode -p \"analyse project\""),
            ParseResult::Ok(Command::AddAgent {
                agent: "claude".to_string(),
                image: None,
                tag: None,
                project: None,
                branch: None,
                name: None,
                auto_mode: true,
                interactive: false,
                prompt: Some("analyse project".to_string()),
                model: None,
                use_env: vec![],
                env_file: None,
                run_as_root: false,
            })
        );
    }

    #[test]
    fn test_parse_add_auto_mode_without_prompt_fails() {
        let result = parse_command_verbose("/add claude --auto-mode");
        match result {
            ParseResult::Err(msg) => {
                assert!(msg.contains("--prompt is required"));
            }
            other => panic!("expected Err, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_add_with_use_env_keys() {
        assert_eq!(
            parse_command_verbose("/add claude --use-env OPENAI_API_KEY --use-env GITHUB_TOKEN"),
            ParseResult::Ok(Command::AddAgent {
                agent: "claude".to_string(),
                image: None,
                tag: None,
                project: None,
                branch: None,
                name: None,
                auto_mode: false,
                interactive: false,
                prompt: None,
                model: None,
                use_env: vec!["OPENAI_API_KEY".to_string(), "GITHUB_TOKEN".to_string()],
                env_file: None,
                run_as_root: false,
            })
        );
    }

    #[test]
    fn test_parse_add_use_env_missing_value() {
        let result = parse_command_verbose("/add claude --use-env");
        assert!(matches!(result, ParseResult::Err(msg) if msg.contains("--use-env requires a key name")));
    }

    #[test]
    fn test_parse_add_with_env_file() {
        assert_eq!(
            parse_command_verbose("/add claude --env-file .env.local"),
            ParseResult::Ok(Command::AddAgent {
                agent: "claude".to_string(),
                image: None,
                tag: None,
                project: None,
                branch: None,
                name: None,
                auto_mode: false,
                interactive: false,
                prompt: None,
                model: None,
                use_env: vec![],
                env_file: Some(".env.local".to_string()),
                run_as_root: false,
            })
        );
    }

    #[test]
    fn test_parse_add_env_file_missing_value() {
        let result = parse_command_verbose("/add claude --env-file");
        assert!(matches!(result, ParseResult::Err(msg) if msg.contains("--env-file requires a path")));
    }

    #[test]
    fn test_parse_add_prompt_flag() {
        assert_eq!(
            parse_command_verbose("/add codex --auto-mode --prompt fix the bug"),
            ParseResult::Ok(Command::AddAgent {
                agent: "codex".to_string(),
                image: None,
                tag: None,
                project: None,
                branch: None,
                name: None,
                auto_mode: true,
                interactive: false,
                prompt: Some("fix the bug".to_string()),
                model: None,
                use_env: vec![],
                env_file: None,
                run_as_root: false,
            })
        );
    }

    #[test]
    fn test_parse_branches() {
        assert_eq!(parse_command("/branches"), Some(Command::Branches));
    }

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
    fn test_parse_open_no_arg() {
        assert_eq!(parse_command("/open"), Some(Command::Open { target: None }));
    }

    #[test]
    fn test_parse_open_with_name() {
        assert_eq!(
            parse_command("/open claude"),
            Some(Command::Open { target: Some("claude".to_string()) })
        );
    }

    #[test]
    fn test_parse_edit_default() {
        assert_eq!(parse_command("/edit"), Some(Command::Edit { tool: None }));
    }

    #[test]
    fn test_parse_edit_specific_tool() {
        assert_eq!(
            parse_command("/edit gitui"),
            Some(Command::Edit { tool: Some("gitui".to_string()) })
        );
    }

    #[test]
    fn test_parse_close_no_arg() {
        assert_eq!(
            parse_command("/close"),
            Some(Command::Close { target: None }),
        );
    }

    #[test]
    fn test_parse_close_with_name() {
        assert_eq!(
            parse_command("/close claude"),
            Some(Command::Close { target: Some("claude".to_string()) }),
        );
    }

    // ===== Skills command tests =====

    #[test]
    fn test_parse_skills_list() {
        assert_eq!(parse_command("/skills"), Some(Command::SkillsList));
        assert_eq!(parse_command("/skills list"), Some(Command::SkillsList));
    }

    #[test]
    fn test_removed_skills_subcommands_are_not_commands() {
        for input in ["/skills add tdd", "/skills remove tdd"] {
            let result = parse_command_verbose(input);
            match result {
                ParseResult::Err(msg) => assert!(
                    msg.contains("Unknown skills subcommand"),
                    "{} should be rejected as an unknown subcommand, got: {}",
                    input,
                    msg
                ),
                other => panic!("expected Err for {}, got {:?}", input, other),
            }
        }
    }

    #[test]
    fn test_parse_skills_show() {
        assert_eq!(
            parse_command("/skills show git-workflow"),
            Some(Command::SkillsShow { name: "git-workflow".to_string() })
        );
    }

    #[test]
    fn test_parse_skills_show_missing_name() {
        let result = parse_command_verbose("/skills show");
        assert!(matches!(result, ParseResult::Err(_)));
    }

    #[test]
    fn test_parse_skills_unknown_subcommand() {
        let result = parse_command_verbose("/skills foo");
        match result {
            ParseResult::Err(msg) => {
                assert!(msg.contains("Unknown skills subcommand"));
                assert!(msg.contains("foo"));
            }
            other => panic!("expected Err, got {:?}", other),
        }
    }

    // ===== Agent command tests =====

    #[test]
    fn test_parse_agent_show() {
        assert_eq!(parse_command("/agent"), Some(Command::AgentShow));
    }

    #[test]
    fn test_removed_agent_set_is_not_a_command() {
        for input in ["/agent set python-developer", "/agent set"] {
            let result = parse_command_verbose(input);
            match result {
                ParseResult::Err(msg) => assert!(
                    msg.contains("Unknown agent subcommand"),
                    "{} should be rejected as an unknown subcommand, got: {}",
                    input,
                    msg
                ),
                other => panic!("expected Err for {}, got {:?}", input, other),
            }
        }
    }

    #[test]
    fn test_parse_agent_list() {
        assert_eq!(parse_command("/agent list"), Some(Command::AgentList));
    }

    #[test]
    fn test_parse_agent_info() {
        assert_eq!(
            parse_command("/agent show rust-developer"),
            Some(Command::AgentInfo { name: "rust-developer".to_string() })
        );
    }

    #[test]
    fn test_parse_agent_show_missing_name() {
        let result = parse_command_verbose("/agent show");
        assert!(matches!(result, ParseResult::Err(_)));
    }

    #[test]
    fn test_parse_agent_unknown_subcommand() {
        let result = parse_command_verbose("/agent foo");
        match result {
            ParseResult::Err(msg) => {
                assert!(msg.contains("Unknown agent subcommand"));
                assert!(msg.contains("foo"));
            }
            other => panic!("expected Err, got {:?}", other),
        }
    }

    #[test]
    fn test_autocomplete_skills() {
        let suggestions = autocomplete("/sk");
        assert!(suggestions.iter().any(|s| s.starts_with("/skills")));
    }

    #[test]
    fn test_autocomplete_agent() {
        let suggestions = autocomplete("/ag");
        assert!(suggestions.iter().any(|s| s.starts_with("/agent")));
    }

    #[test]
    fn test_parse_add_with_model() {
        assert_eq!(
            parse_command_verbose("/add claude --model claude-sonnet-4-5-20250929"),
            ParseResult::Ok(Command::AddAgent {
                agent: "claude".to_string(),
                image: None,
                tag: None,
                project: None,
                branch: None,
                name: None,
                auto_mode: false,
                interactive: false,
                prompt: None,
                model: Some("claude-sonnet-4-5-20250929".to_string()),
                use_env: vec![],
                env_file: None,
                run_as_root: false,
            })
        );
    }

    #[test]
    fn test_parse_add_model_missing_value() {
        let result = parse_command_verbose("/add claude --model");
        match result {
            ParseResult::Err(msg) => {
                assert!(msg.contains("--model requires a value"));
            }
            other => panic!("expected Err, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_add_with_model_and_auto_mode() {
        assert_eq!(
            parse_command_verbose("/add claude --model claude-opus-4-20250514 --auto-mode -p do stuff"),
            ParseResult::Ok(Command::AddAgent {
                agent: "claude".to_string(),
                image: None,
                tag: None,
                project: None,
                branch: None,
                name: None,
                auto_mode: true,
                interactive: false,
                prompt: Some("do stuff".to_string()),
                model: Some("claude-opus-4-20250514".to_string()),
                use_env: vec![],
                env_file: None,
                run_as_root: false,
            })
        );
    }

    #[test]
    fn test_parse_destroy() {
        assert_eq!(parse_command("/destroy"), Some(Command::Destroy));
    }

    #[test]
    fn test_autocomplete_destroy() {
        let suggestions = autocomplete("/des");
        assert!(suggestions.iter().any(|s| s == "/destroy"));
    }

    #[test]
    fn test_parse_add_interactive() {
        assert_eq!(
            parse_command_verbose("/add claude --interactive"),
            ParseResult::Ok(Command::AddAgent {
                agent: "claude".to_string(),
                image: None,
                tag: None,
                project: None,
                branch: None,
                name: None,
                auto_mode: false,
                interactive: true,
                prompt: None,
                model: None,
                use_env: vec![],
                env_file: None,
                run_as_root: false,
            })
        );
    }

    #[test]
    fn test_parse_add_defaults_interactive_false() {
        let parsed = parse_command_verbose("/add claude");
        assert!(matches!(
            parsed,
            ParseResult::Ok(Command::AddAgent { interactive: false, .. })
        ));
    }

    #[test]
    fn test_parse_add_interactive_and_auto_mode_rejected() {
        let result = parse_command_verbose("/add claude --interactive --auto-mode -p hi");
        match result {
            ParseResult::Err(msg) => assert!(msg.contains("mutually exclusive")),
            other => panic!("expected Err, got {:?}", other),
        }
    }

    // ===== Help/parser parity (Epic 2 AC1) =====

    /// Materialize a help pattern into a concrete command string by replacing
    /// each `<...>`/`[--flag ...]` placeholder with a plausible token, so the
    /// parser is exercised at the SUBCOMMAND level (not just the base token).
    fn concrete_command(pattern: &str) -> String {
        let mut out = String::new();
        for tok in pattern.split_whitespace() {
            if out.is_empty() {
                out.push_str(tok);
            } else if tok.starts_with('<') {
                out.push_str(" X");
            } else if tok.starts_with('[') {
                // Optional-arg groups: keep only the bare subcommand forms we
                // can feed (e.g. "[on|off|now]" -> "on"); skip flag groups.
                if tok.contains('|') && !tok.contains("--") {
                    let first = tok
                        .trim_matches(|c| c == '[' || c == ']')
                        .split('|')
                        .next()
                        .unwrap_or("")
                        .trim();
                    if !first.is_empty() {
                        out.push(' ');
                        out.push_str(first);
                    }
                }
            } else if !tok.starts_with("--") {
                out.push(' ');
                out.push_str(tok);
            }
        }
        out
    }

    /// True if `input` is recognized by the parser: an `Ok`, or an `Err` that is
    /// NOT "Unknown command"/"Unknown <x> subcommand" (i.e. not a rejected verb).
    fn command_is_recognized(input: &str) -> bool {
        match parse_command_verbose(input) {
            ParseResult::Ok(_) => true,
            ParseResult::Err(msg) => {
                !msg.contains("Unknown command") && !msg.contains("subcommand")
            }
            ParseResult::NotACommand => false,
        }
    }

    #[test]
    fn test_help_entries_are_all_recognized_commands() {
        for entry in HELP_ENTRIES {
            if !entry.pattern.starts_with('/') {
                continue;
            }
            let concrete = concrete_command(entry.pattern);
            assert!(
                command_is_recognized(&concrete),
                "help advertises '{}' (as '{}') but it is not a recognized command",
                entry.pattern,
                concrete
            );
        }
    }

    #[test]
    fn test_help_subcommands_are_not_unknown() {
        // A removed/never-implemented subcommand in the help table must fail
        // this test (this is what the base-token check could not catch).
        for entry in HELP_ENTRIES {
            let concrete = concrete_command(entry.pattern);
            let result = parse_command_verbose(&concrete);
            if let ParseResult::Err(msg) = result {
                assert!(
                    !msg.contains("Unknown") || !msg.contains("subcommand"),
                    "help advertises '{}' but the parser rejects it as unknown: {}",
                    entry.pattern,
                    msg
                );
            }
        }
    }

    #[test]
    fn test_help_advertises_no_removed_subcommands() {
        let help = format_help();
        for removed in [
            "/mcp add",
            "/mcp remove",
            "/mcp enable",
            "/mcp disable",
            "/skills add",
            "/skills remove",
            "/agent set",
        ] {
            assert!(
                !help.contains(removed),
                "help still advertises removed command '{}'",
                removed
            );
        }
    }

    #[test]
    fn test_help_mentions_declarative_config() {
        assert!(format_help().contains("sandbox.yml"));
    }

    #[test]
    fn test_autocomplete_entries_are_all_recognized_commands() {
        for cmd in ALL_COMMANDS {
            assert!(
                command_is_recognized(cmd),
                "autocomplete advertises '{}' but it is not a recognized command",
                cmd
            );
        }
    }
}
