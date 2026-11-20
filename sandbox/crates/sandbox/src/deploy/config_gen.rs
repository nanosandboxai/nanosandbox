//! MCP config generation — ported from the legacy in-VM Go gateway (mcp/config_gen.go).
//!
//! Generates per-agent MCP server configuration files that are mounted
//! read-only into the guest VM. Each agent type uses a different config format:
//!
//! | Agent    | Format | Path                             |
//! |----------|--------|----------------------------------|
//! | Claude   | JSON   | ~/.claude/.claude.json           |
//! | Codex    | TOML   | ~/.codex/config.toml             |
//! | Goose    | YAML   | ~/.config/goose/config.yaml      |
//! | Cursor   | JSON   | ~/.cursor/mcp.json               |
//! | OpenCode | JSON   | ~/.config/opencode/opencode.json |
//! | Gemini   | JSON   | ~/.gemini/settings.json          |
//! | Crush    | JSON   | ~/.config/crush/crush.json       |
//! | Aider    | —      | (no native MCP support)          |

use crate::config::{AgentType, McpServerConfig};
use std::collections::HashMap;
use tracing::warn;

/// Format identifier for MCP config generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpConfigFormat {
    Claude,
    Codex,
    Goose,
    Cursor,
    Opencode,
    Aider,
    Gemini,
    Crush,
}

impl From<AgentType> for McpConfigFormat {
    fn from(at: AgentType) -> Self {
        match at {
            AgentType::Claude => McpConfigFormat::Claude,
            AgentType::Codex => McpConfigFormat::Codex,
            AgentType::Goose => McpConfigFormat::Goose,
            AgentType::Cursor => McpConfigFormat::Cursor,
            AgentType::Opencode => McpConfigFormat::Opencode,
            AgentType::Aider => McpConfigFormat::Aider,
            AgentType::Gemini => McpConfigFormat::Gemini,
            AgentType::Crush => McpConfigFormat::Crush,
        }
    }
}

/// A config file to be written to the host-side config directory.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    /// Relative path under the config directory (e.g. "claude/settings.json").
    pub relative_path: String,
    /// File contents as bytes.
    pub content: Vec<u8>,
}

/// MCP config generator.
pub struct ConfigGenerator;

impl ConfigGenerator {
    /// Generate all MCP config files for a given agent type.
    ///
    /// `servers` is the map of MCP server name → config.
    /// Returns a list of config files to write.
    pub fn generate_all(
        servers: &HashMap<String, McpServerConfig>,
        agent_type: &AgentType,
    ) -> Vec<ConfigFile> {
        let format = McpConfigFormat::from(*agent_type);
        let mut files = Vec::new();

        match format {
            McpConfigFormat::Claude => {
                files.push(ConfigFile {
                    relative_path: "claude/.claude.json".to_string(),
                    content: Self::claude_config_json(servers).into_bytes(),
                });
            }
            McpConfigFormat::Codex => {
                files.push(ConfigFile {
                    relative_path: "codex/config.toml".to_string(),
                    content: Self::codex_config_toml(servers).into_bytes(),
                });
            }
            McpConfigFormat::Goose => {
                if let Some(content) = Self::generate_goose(servers) {
                    files.push(ConfigFile {
                        relative_path: "goose/config.yaml".to_string(),
                        content,
                    });
                }
            }
            McpConfigFormat::Cursor => {
                if let Some(content) = Self::generate_cursor(servers) {
                    files.push(ConfigFile {
                        relative_path: "cursor/mcp.json".to_string(),
                        content,
                    });
                }
            }
            McpConfigFormat::Opencode => {
                if let Some(content) = Self::generate_opencode(servers) {
                    files.push(ConfigFile {
                        relative_path: "opencode/opencode.json".to_string(),
                        content,
                    });
                }
            }
            McpConfigFormat::Aider => {
                // Aider has no native MCP client support, so no MCP config file
                // is written for it. It still receives skills via CONVENTIONS.md
                // (see skills_gen), but mounting mcp_servers there would be ignored.
            }
            McpConfigFormat::Gemini => {
                if let Some(content) = Self::generate_gemini(servers) {
                    files.push(ConfigFile {
                        relative_path: "gemini/settings.json".to_string(),
                        content,
                    });
                }
            }
            McpConfigFormat::Crush => {
                if let Some(content) = Self::generate_crush(servers) {
                    files.push(ConfigFile {
                        relative_path: "crush/crush.json".to_string(),
                        content,
                    });
                }
            }
        }

        files
    }

    /// Build the stdio entry for one MCP server.
    ///
    /// `with_type` adds `"type": "stdio"` — Cursor and Crush require it; Claude
    /// and Gemini infer stdio and accept the entry without it.
    fn stdio_entry(srv: &McpServerConfig, with_type: bool) -> serde_json::Value {
        let mut entry = serde_json::Map::new();
        if with_type {
            entry.insert(
                "type".to_string(),
                serde_json::Value::String("stdio".to_string()),
            );
        }
        entry.insert(
            "command".to_string(),
            serde_json::Value::String(srv.command.clone()),
        );
        entry.insert(
            "args".to_string(),
            serde_json::Value::Array(
                srv.args
                    .iter()
                    .map(|a| serde_json::Value::String(a.clone()))
                    .collect(),
            ),
        );
        let env_map: serde_json::Map<String, _> = srv
            .env
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        if !env_map.is_empty() {
            entry.insert("env".to_string(), serde_json::Value::Object(env_map));
        }
        serde_json::Value::Object(entry)
    }

    /// Build a sorted `mcpServers`/`mcp` object for a set of servers.
    fn stdio_server_map(
        servers: &HashMap<String, McpServerConfig>,
        with_type: bool,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut entries = serde_json::Map::new();
        let mut names: Vec<&String> = servers.keys().collect();
        names.sort();
        for name in names {
            let srv = &servers[name];
            warn_inline_secrets(name, &srv.env);
            entries.insert(name.clone(), Self::stdio_entry(srv, with_type));
        }
        entries
    }

    /// Claude Code user config written to `$CLAUDE_CONFIG_DIR/.claude.json`
    /// (`~/.claude/.claude.json`). Claude reads user-scope MCP servers from the
    /// top-level `mcpServers` key here — `mcpServers` inside `settings.json` is
    /// silently ignored. Also carries the workspace-trust entry so the
    /// "do you trust the files in this folder?" prompt is skipped.
    fn claude_config_json(servers: &HashMap<String, McpServerConfig>) -> String {
        let mut doc = serde_json::Map::new();
        if !servers.is_empty() {
            doc.insert(
                "mcpServers".to_string(),
                serde_json::Value::Object(Self::stdio_server_map(servers, false)),
            );
        }
        doc.insert(
            "projects".to_string(),
            serde_json::json!({ "/home/developer": { "hasTrustDialogAccepted": true } }),
        );
        serde_json::to_string_pretty(&serde_json::Value::Object(doc))
            .unwrap_or_else(|_| "{}".to_string())
    }

    /// Codex `~/.codex/config.toml`. Codex reads `[mcp_servers.<name>]` from this
    /// same file (there is no separate `mcp_servers.toml`), so the MCP sections
    /// and the workspace-trust section are emitted together.
    fn codex_config_toml(servers: &HashMap<String, McpServerConfig>) -> String {
        let mut output =
            String::from("[projects.\"/home/developer\"]\ntrust_level = \"trusted\"\n");

        let mut names: Vec<&String> = servers.keys().collect();
        names.sort();

        for name in names {
            let srv = &servers[name];
            warn_inline_secrets(name, &srv.env);
            output.push('\n');
            output.push_str(&format!("[mcp_servers.{}]\n", name));
            output.push_str(&format!("command = {:?}\n", srv.command));

            output.push_str("args = [");
            for (j, arg) in srv.args.iter().enumerate() {
                if j > 0 {
                    output.push_str(", ");
                }
                output.push_str(&format!("{:?}", arg));
            }
            output.push_str("]\n");

            let non_empty_env: HashMap<_, _> =
                srv.env.iter().filter(|(_, v)| !v.is_empty()).collect();
            if !non_empty_env.is_empty() {
                output.push_str("env = { ");
                let mut env_keys: Vec<&String> = non_empty_env.keys().copied().collect();
                env_keys.sort();
                for (j, k) in env_keys.iter().enumerate() {
                    if j > 0 {
                        output.push_str(", ");
                    }
                    output.push_str(&format!("{:?} = {:?}", k, non_empty_env[*k]));
                }
                output.push_str(" }\n");
            }
        }

        output
    }

    /// Generate Goose MCP config (YAML-like key-value format).
    ///
    /// Format:
    /// ```yaml
    /// GOOSE_TELEMETRY_ENABLED: false
    ///
    /// extensions:
    ///   github:
    ///     name: github
    ///     cmd: npx
    ///     args:
    ///       - "-y"
    ///       - "@modelcontextprotocol/server-github"
    ///     envs:
    ///       GITHUB_TOKEN: "..."
    ///     type: stdio
    ///     enabled: true
    ///     timeout: 300
    /// ```
    fn generate_goose(servers: &HashMap<String, McpServerConfig>) -> Option<Vec<u8>> {
        // Always write Goose's config, even with no MCP servers. The
        // `GOOSE_TELEMETRY_ENABLED: false` line is what suppresses Goose's
        // first-run "Would you like to send anonymous data?" prompt. If that
        // prompt is shown, accepting it makes Goose restart its session, and
        // the TUI then sees the agent's PTY exit and tears down the sandbox.
        let mut output = String::new();
        output.push_str("GOOSE_TELEMETRY_ENABLED: false\n\n");

        if servers.is_empty() {
            return Some(output.into_bytes());
        }

        output.push_str("extensions:\n");

        let mut names: Vec<&String> = servers.keys().collect();
        names.sort();

        for name in names {
            let srv = &servers[name];
            output.push_str(&format!("  {}:\n", name));
            output.push_str(&format!("    name: {}\n", name));
            output.push_str(&format!("    cmd: {}\n", srv.command));

            if !srv.args.is_empty() {
                output.push_str("    args:\n");
                for arg in &srv.args {
                    output.push_str(&format!("      - {:?}\n", arg));
                }
            }

            let non_empty_env: HashMap<_, _> =
                srv.env.iter().filter(|(_, v)| !v.is_empty()).collect();
            if !non_empty_env.is_empty() {
                warn_inline_secrets(name, &srv.env);
                output.push_str("    envs:\n");
                let mut env_keys: Vec<&String> = non_empty_env.keys().copied().collect();
                env_keys.sort();
                for k in env_keys {
                    output.push_str(&format!("      {}: {:?}\n", k, non_empty_env[k]));
                }
            }

            output.push_str("    type: stdio\n");
            output.push_str("    enabled: true\n");
            output.push_str("    timeout: 300\n");
        }

        Some(output.into_bytes())
    }

    /// Cursor `~/.cursor/mcp.json` — `mcpServers` with explicit `type: stdio`.
    fn generate_cursor(servers: &HashMap<String, McpServerConfig>) -> Option<Vec<u8>> {
        if servers.is_empty() {
            return None;
        }
        let doc = serde_json::json!({
            "mcpServers": serde_json::Value::Object(Self::stdio_server_map(servers, true))
        });
        serde_json::to_string_pretty(&doc)
            .ok()
            .map(String::into_bytes)
    }

    /// Generate OpenCode MCP config (JSON, `mcp` key in opencode.json).
    ///
    /// Format:
    /// ```json
    /// {
    ///   "mcp": {
    ///     "github": {
    ///       "type": "local",
    ///       "command": ["npx", "-y", "@modelcontextprotocol/server-github"],
    ///       "enabled": true,
    ///       "environment": { "GITHUB_TOKEN": "..." }
    ///     }
    ///   }
    /// }
    /// ```
    fn generate_opencode(servers: &HashMap<String, McpServerConfig>) -> Option<Vec<u8>> {
        if servers.is_empty() {
            return None;
        }

        let mut entries = serde_json::Map::new();
        let mut names: Vec<&String> = servers.keys().collect();
        names.sort();

        for name in names {
            let srv = &servers[name];
            let mut entry = serde_json::Map::new();
            entry.insert(
                "type".to_string(),
                serde_json::Value::String("local".to_string()),
            );
            entry.insert(
                "command".to_string(),
                serde_json::Value::Array(
                    std::iter::once(srv.command.clone())
                        .chain(srv.args.iter().cloned())
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );
            entry.insert(
                "disabled".to_string(),
                serde_json::Value::Bool(!srv.enabled),
            );
            if !srv.env.is_empty() {
                warn_inline_secrets(name, &srv.env);
                let env_map: serde_json::Map<String, _> = srv
                    .env
                    .iter()
                    .filter(|(_, v)| !v.is_empty())
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                    .collect();
                if !env_map.is_empty() {
                    entry.insert(
                        "environment".to_string(),
                        serde_json::Value::Object(env_map),
                    );
                }
            }
            entries.insert(name.clone(), serde_json::Value::Object(entry));
        }

        let wrapper = serde_json::json!({ "mcp": serde_json::Value::Object(entries) });
        let content = serde_json::to_string_pretty(&wrapper).ok()?;
        Some(content.into_bytes())
    }

    /// Gemini CLI `~/.gemini/settings.json` — `mcpServers` (stdio inferred).
    fn generate_gemini(servers: &HashMap<String, McpServerConfig>) -> Option<Vec<u8>> {
        if servers.is_empty() {
            return None;
        }
        let doc = serde_json::json!({
            "mcpServers": serde_json::Value::Object(Self::stdio_server_map(servers, false))
        });
        serde_json::to_string_pretty(&doc)
            .ok()
            .map(String::into_bytes)
    }

    /// Crush `~/.config/crush/crush.json` — top-level `mcp` key (not
    /// `mcpServers`), each entry with explicit `type: stdio`.
    fn generate_crush(servers: &HashMap<String, McpServerConfig>) -> Option<Vec<u8>> {
        if servers.is_empty() {
            return None;
        }
        let doc = serde_json::json!({
            "mcp": serde_json::Value::Object(Self::stdio_server_map(servers, true))
        });
        serde_json::to_string_pretty(&doc)
            .ok()
            .map(String::into_bytes)
    }
}

fn looks_like_secret(value: &str) -> bool {
    if value.starts_with('$') {
        return false;
    }
    const PREFIXES: &[&str] = &[
        "AKIA",
        "gho_",
        "ghp_",
        "github_pat_",
        "sk-",
        "sk_",
        "xoxb-",
        "xoxp-",
    ];
    if PREFIXES.iter().any(|p| value.starts_with(p)) {
        return true;
    }
    value.len() > 20 && !value.contains(char::is_whitespace)
}

fn warn_inline_secrets(server_name: &str, env: &HashMap<String, String>) {
    for (key, value) in env {
        if looks_like_secret(value) {
            let hint = if value.len() > 8 {
                format!("{}...{}", &value[..4], &value[value.len() - 4..])
            } else {
                "(short)".to_string()
            };
            warn!(
                server = server_name,
                env_key = key,
                value_hint = %hint,
                "MCP server env contains inline secret — will be written to guest-visible config file"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_servers() -> HashMap<String, McpServerConfig> {
        let mut m = HashMap::new();
        m.insert(
            "github".to_string(),
            McpServerConfig {
                command: "npx".to_string(),
                args: vec![
                    "-y".to_string(),
                    "@modelcontextprotocol/server-github".to_string(),
                ],
                env: [("GITHUB_TOKEN".to_string(), "test-token".to_string())].into(),
                enabled: true,
            },
        );
        m.insert(
            "filesystem".to_string(),
            McpServerConfig {
                command: "npx".to_string(),
                args: vec![
                    "-y".to_string(),
                    "@modelcontextprotocol/server-filesystem".to_string(),
                ],
                env: HashMap::new(),
                enabled: true,
            },
        );
        m
    }

    #[test]
    fn test_generate_claude() {
        let servers = sample_servers();
        let files = ConfigGenerator::generate_all(&servers, &AgentType::Claude);
        assert_eq!(files.len(), 1);

        // MCP servers and workspace trust both live in ~/.claude/.claude.json;
        // Claude silently ignores mcpServers placed in settings.json.
        let cfg = files
            .iter()
            .find(|f| f.relative_path == "claude/.claude.json")
            .expect("claude config");
        let content = String::from_utf8(cfg.content.clone()).unwrap();
        assert!(content.contains("mcpServers"));
        assert!(content.contains("github"));
        assert!(content.contains("npx"));
        assert!(content.contains("GITHUB_TOKEN"));
        assert!(content.contains("filesystem"));
        assert!(content.contains("hasTrustDialogAccepted"));
        assert!(content.contains("/home/developer"));
    }

    #[test]
    fn test_generate_codex() {
        let servers = sample_servers();
        let files = ConfigGenerator::generate_all(&servers, &AgentType::Codex);
        assert_eq!(files.len(), 1);

        // Codex reads [mcp_servers.*] from config.toml, alongside the trust table.
        let cfg = files
            .iter()
            .find(|f| f.relative_path == "codex/config.toml")
            .expect("codex config");
        let content = String::from_utf8(cfg.content.clone()).unwrap();
        assert!(content.contains("[mcp_servers.github]"));
        assert!(content.contains("command = \"npx\""));
        assert!(content.contains("trust_level = \"trusted\""));
        assert!(content.contains("/home/developer"));
    }

    #[test]
    fn test_generate_goose() {
        let servers = sample_servers();
        let files = ConfigGenerator::generate_all(&servers, &AgentType::Goose);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "goose/config.yaml");

        let content = String::from_utf8(files[0].content.clone()).unwrap();
        assert!(content.contains("GOOSE_TELEMETRY_ENABLED: false"));
        assert!(content.contains("extensions:"));
        assert!(content.contains("cmd: npx"));
        assert!(content.contains("type: stdio"));
        assert!(content.contains("enabled: true"));
        assert!(content.contains("timeout: 300"));
    }

    #[test]
    fn test_generate_cursor() {
        let servers = sample_servers();
        let files = ConfigGenerator::generate_all(&servers, &AgentType::Cursor);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "cursor/mcp.json");

        let content = String::from_utf8(files[0].content.clone()).unwrap();
        assert!(content.contains("mcpServers"));
        assert!(content.contains("\"type\": \"stdio\""));
    }

    #[test]
    fn test_generate_opencode() {
        let servers = sample_servers();
        let files = ConfigGenerator::generate_all(&servers, &AgentType::Opencode);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "opencode/opencode.json");

        let content = String::from_utf8(files[0].content.clone()).unwrap();
        assert!(content.contains("\"mcp\""));
        assert!(content.contains("\"type\": \"local\""));
        assert!(content.contains("\"disabled\": false"));
        assert!(content.contains("github"));
        assert!(content.contains("npx"));
        assert!(content.contains("GITHUB_TOKEN"));
    }

    #[test]
    fn test_generate_aider_has_no_mcp() {
        // Aider has no native MCP client, so it must not get an MCP config file.
        let servers = sample_servers();
        let files = ConfigGenerator::generate_all(&servers, &AgentType::Aider);
        assert!(files.is_empty(), "aider must not get an MCP config file");
    }

    #[test]
    fn test_generate_gemini() {
        let servers = sample_servers();
        let files = ConfigGenerator::generate_all(&servers, &AgentType::Gemini);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "gemini/settings.json");

        let content = String::from_utf8(files[0].content.clone()).unwrap();
        assert!(content.contains("mcpServers"));
        assert!(content.contains("github"));
    }

    #[test]
    fn test_generate_crush() {
        let servers = sample_servers();
        let files = ConfigGenerator::generate_all(&servers, &AgentType::Crush);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "crush/crush.json");

        // Crush uses a top-level `mcp` key with explicit `type: stdio`.
        let content = String::from_utf8(files[0].content.clone()).unwrap();
        assert!(content.contains("\"mcp\""));
        assert!(!content.contains("mcpServers"));
        assert!(content.contains("\"type\": \"stdio\""));
        assert!(content.contains("github"));
    }

    #[test]
    fn test_generate_empty_servers() {
        let servers = HashMap::new();
        let files = ConfigGenerator::generate_all(&servers, &AgentType::Opencode);
        assert!(files.is_empty(), "empty servers should produce no files");
    }

    #[test]
    fn test_generate_claude_without_servers_still_writes_trust() {
        let servers = HashMap::new();
        let files = ConfigGenerator::generate_all(&servers, &AgentType::Claude);
        assert_eq!(
            files.len(),
            1,
            "claude must always write .claude.json so the workspace is pre-trusted"
        );
        assert_eq!(files[0].relative_path, "claude/.claude.json");
        let content = String::from_utf8(files[0].content.clone()).unwrap();
        assert!(content.contains("hasTrustDialogAccepted"));
    }

    #[test]
    fn test_generate_goose_without_servers_still_disables_telemetry() {
        let servers = HashMap::new();
        let files = ConfigGenerator::generate_all(&servers, &AgentType::Goose);
        assert_eq!(
            files.len(),
            1,
            "goose must always write config.yaml so telemetry stays disabled"
        );
        let content = String::from_utf8(files[0].content.clone()).unwrap();
        assert!(content.contains("GOOSE_TELEMETRY_ENABLED: false"));
        assert!(!content.contains("extensions:"));
    }

    #[test]
    fn test_generate_goose_empty_env_omitted() {
        let mut servers = HashMap::new();
        servers.insert(
            "test".to_string(),
            McpServerConfig {
                command: "uvx".to_string(),
                args: vec!["mcp-server-fetch".to_string()],
                env: HashMap::new(),
                enabled: true,
            },
        );
        let files = ConfigGenerator::generate_all(&servers, &AgentType::Goose);
        let content = String::from_utf8(files[0].content.clone()).unwrap();
        assert!(
            !content.contains("envs:"),
            "empty env should not produce envs section"
        );
    }

    #[test]
    fn test_generate_claude_sorted_keys() {
        let mut servers = HashMap::new();
        servers.insert(
            "z_last".to_string(),
            McpServerConfig {
                command: "echo".to_string(),
                args: vec![],
                env: HashMap::new(),
                enabled: true,
            },
        );
        servers.insert(
            "a_first".to_string(),
            McpServerConfig {
                command: "echo".to_string(),
                args: vec![],
                env: HashMap::new(),
                enabled: true,
            },
        );
        let files = ConfigGenerator::generate_all(&servers, &AgentType::Claude);
        let content = String::from_utf8(files[0].content.clone()).unwrap();
        let a_pos = content.find("a_first").unwrap();
        let z_pos = content.find("z_last").unwrap();
        assert!(a_pos < z_pos, "keys should be sorted alphabetically");
    }

    #[test]
    fn test_looks_like_secret_sk_prefix() {
        assert!(looks_like_secret("sk-ant-abcdefghijklmnopqrstuvwxyz"));
    }

    #[test]
    fn test_looks_like_secret_var_ref() {
        assert!(!looks_like_secret("$ANTHROPIC_API_KEY"));
    }

    #[test]
    fn test_looks_like_secret_var_ref_braces() {
        assert!(!looks_like_secret("${ANTHROPIC_API_KEY}"));
    }

    #[test]
    fn test_looks_like_secret_ghp() {
        assert!(looks_like_secret("ghp_abcdefghijklmnopqrstuvwxyz123456"));
    }

    #[test]
    fn test_looks_like_secret_gho() {
        assert!(looks_like_secret("gho_abcdefghijklmnopqrstuvwxyz123456"));
    }

    #[test]
    fn test_looks_like_secret_github_pat() {
        assert!(looks_like_secret("github_pat_abcdefghijklmnopqrstuvwxyz"));
    }

    #[test]
    fn test_looks_like_secret_akid() {
        assert!(looks_like_secret("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn test_looks_like_secret_slack_bot() {
        assert!(looks_like_secret(
            "xoxb-1234567890-1234567890123-abc123def456"
        ));
    }

    #[test]
    fn test_looks_like_secret_slack_user() {
        assert!(looks_like_secret(
            "xoxp-1234567890-1234567890123-abc123def456"
        ));
    }

    #[test]
    fn test_looks_like_secret_long_token() {
        assert!(looks_like_secret("abcdefghijklmnopqrstuvwxyz123456"));
    }

    #[test]
    fn test_looks_like_secret_short_value() {
        assert!(!looks_like_secret("hello"));
    }

    #[test]
    fn test_looks_like_secret_empty() {
        assert!(!looks_like_secret(""));
    }

    #[test]
    fn test_looks_like_secret_sk_underscore() {
        assert!(looks_like_secret("sk_abcdefghijklmnopqrstuvwxyz"));
    }
}
