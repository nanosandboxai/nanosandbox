//! Slash command parsing and autocomplete.

/// Parsed slash command.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Exit the TUI.
    Quit,
    /// Show help text.
    Help,
    /// Clear the current panel output.
    Clear,
    /// Close the focused panel.
    Close,
    /// Add a new agent panel, optionally with a custom image.
    AddAgent {
        /// Agent name.
        agent: String,
        /// Optional custom container image.
        image: Option<String>,
        /// Optional project path to mount.
        project: Option<String>,
        /// Optional branch name for the project clone.
        branch: Option<String>,
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
    /// Add a new MCP server configuration.
    McpAdd {
        /// Server name.
        name: String,
        /// Command to run.
        command: String,
        /// Arguments for the command.
        args: Vec<String>,
    },
    /// Remove an MCP server by name.
    McpRemove {
        /// Server name.
        name: String,
    },
    /// Enable an MCP server by name.
    McpEnable {
        /// Server name.
        name: String,
    },
    /// Disable an MCP server by name.
    McpDisable {
        /// Server name.
        name: String,
    },
    /// Set or list environment variables for the focused panel.
    Env {
        /// KEY=VALUE pair to set, or None to list current env vars.
        assignment: Option<(String, String)>,
    },
    /// Kill (destroy) a sandbox and remove its panel.
    Kill {
        /// 1-indexed panel number, or None to kill focused panel.
        panel: Option<usize>,
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
    Open {
        /// Tool override, or None for preferred/auto-detected.
        tool: Option<String>,
    },
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
const SUPPORTED_AGENTS: &[&str] = &["claude", "opencode", "goose", "codex", "cursor"];

const ALL_COMMANDS: &[&str] = &[
    "/quit", "/q", "/help", "/clear", "/close", "/copy",
    "/add", "/focus", "/kill", "/reconnect", "/env",
    "/zoom", "/branches",
    "/gitsync", "/gitsync on", "/gitsync off", "/gitsync now",
    "/open",
    "/sandboxes",
    "/mcp", "/mcp list", "/mcp add", "/mcp remove", "/mcp enable", "/mcp disable",
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
        "/help" => ParseResult::Ok(Command::Help),
        "/clear" => ParseResult::Ok(Command::Clear),
        "/close" => ParseResult::Ok(Command::Close),

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
            let tool = parts.get(1).map(|s| s.to_string());
            ParseResult::Ok(Command::Open { tool })
        }

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
                "Usage: /add <agent> [--image <image>] [--project <path>] [--branch <name>]\n\
                 Supported agents: {}\n\
                 Example: /add claude",
                SUPPORTED_AGENTS.join(", "),
            ));
        }
    };

    let mut image = None;
    let mut project = None;
    let mut branch = None;
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
                    Some(v) => { project = Some(v.to_string()); i += 2; }
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
            other => {
                return ParseResult::Err(format!(
                    "Unknown option: {}\n\
                     Usage: /add <agent> [--image <image>] [--project <path>] [--branch <name>]",
                    other,
                ));
            }
        }
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
        project,
        branch,
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
        Some("add") => {
            let name = match parts.get(2) {
                Some(n) => n.to_string(),
                None => {
                    return ParseResult::Err(
                        "Usage: /mcp add <name> <command> [args...]\n\
                         Example: /mcp add github npx @github/mcp-server"
                            .to_string(),
                    );
                }
            };
            let command = match parts.get(3) {
                Some(c) => c.to_string(),
                None => {
                    return ParseResult::Err(format!(
                        "Missing command for MCP server '{}'.\n\
                         Usage: /mcp add <name> <command> [args...]\n\
                         Example: /mcp add {} npx @some/mcp-server",
                        name, name,
                    ));
                }
            };
            let args: Vec<String> = parts[4..].iter().map(|s| s.to_string()).collect();
            ParseResult::Ok(Command::McpAdd { name, command, args })
        }
        Some("remove") => match parts.get(2) {
            Some(name) => ParseResult::Ok(Command::McpRemove {
                name: name.to_string(),
            }),
            None => ParseResult::Err(
                "Usage: /mcp remove <name>\n\
                 Use /mcp list to see configured servers."
                    .to_string(),
            ),
        },
        Some("enable") => match parts.get(2) {
            Some(name) => ParseResult::Ok(Command::McpEnable {
                name: name.to_string(),
            }),
            None => ParseResult::Err(
                "Usage: /mcp enable <name>\n\
                 Use /mcp list to see configured servers."
                    .to_string(),
            ),
        },
        Some("disable") => match parts.get(2) {
            Some(name) => ParseResult::Ok(Command::McpDisable {
                name: name.to_string(),
            }),
            None => ParseResult::Err(
                "Usage: /mcp disable <name>\n\
                 Use /mcp list to see configured servers."
                    .to_string(),
            ),
        },
        Some(sub) => ParseResult::Err(format!(
            "Unknown MCP subcommand: '{}'\n\
             Available: /mcp list, /mcp add, /mcp remove, /mcp enable, /mcp disable",
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
        Some(n) => match n.parse::<usize>() {
            Ok(panel) => ParseResult::Ok(Command::Kill { panel: Some(panel) }),
            Err(_) => ParseResult::Err(format!(
                "'{}' is not a valid panel number.\n\
                 Usage: /kill [n]  (0-indexed, or omit for focused panel)\n\
                 Example: /kill 0",
                n,
            )),
        },
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
                project: None,
                branch: None,
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
                project: None,
                branch: None,
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
    fn test_parse_mcp_add() {
        assert_eq!(
            parse_command("/mcp add github npx @github/mcp-server"),
            Some(Command::McpAdd {
                name: "github".to_string(),
                command: "npx".to_string(),
                args: vec!["@github/mcp-server".to_string()],
            })
        );
    }

    #[test]
    fn test_parse_mcp_remove() {
        assert_eq!(
            parse_command("/mcp remove github"),
            Some(Command::McpRemove {
                name: "github".to_string(),
            })
        );
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
                project: None,
                branch: None,
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
    fn test_mcp_add_missing_args_shows_help() {
        let result = parse_command_verbose("/mcp add");
        match result {
            ParseResult::Err(msg) => assert!(msg.contains("Usage:")),
            other => panic!("expected Err, got {:?}", other),
        }
    }

    #[test]
    fn test_mcp_add_missing_command_shows_help() {
        let result = parse_command_verbose("/mcp add github");
        match result {
            ParseResult::Err(msg) => {
                assert!(msg.contains("Missing command"));
                assert!(msg.contains("github"));
            }
            other => panic!("expected Err, got {:?}", other),
        }
    }

    #[test]
    fn test_mcp_remove_missing_name_shows_help() {
        let result = parse_command_verbose("/mcp remove");
        match result {
            ParseResult::Err(msg) => assert!(msg.contains("Usage:")),
            other => panic!("expected Err, got {:?}", other),
        }
    }

    #[test]
    fn test_mcp_enable_missing_name_shows_help() {
        let result = parse_command_verbose("/mcp enable");
        match result {
            ParseResult::Err(msg) => assert!(msg.contains("Usage:")),
            other => panic!("expected Err, got {:?}", other),
        }
    }

    #[test]
    fn test_mcp_disable_missing_name_shows_help() {
        let result = parse_command_verbose("/mcp disable");
        match result {
            ParseResult::Err(msg) => assert!(msg.contains("Usage:")),
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
            ParseResult::Ok(Command::Kill { panel: Some(2) }),
        );
    }

    #[test]
    fn test_parse_kill_invalid_number() {
        let result = parse_command_verbose("/kill abc");
        assert!(matches!(result, ParseResult::Err(_)));
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
        assert_eq!(
            parse_command_verbose("/add claude --project /tmp/myapp"),
            ParseResult::Ok(Command::AddAgent {
                agent: "claude".to_string(),
                image: None,
                project: Some("/tmp/myapp".to_string()),
                branch: None,
            })
        );
    }

    #[test]
    fn test_parse_add_with_project_and_branch() {
        assert_eq!(
            parse_command_verbose("/add claude --project /tmp/myapp --branch feat/auth"),
            ParseResult::Ok(Command::AddAgent {
                agent: "claude".to_string(),
                image: None,
                project: Some("/tmp/myapp".to_string()),
                branch: Some("feat/auth".to_string()),
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
}
