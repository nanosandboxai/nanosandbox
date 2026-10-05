//! MCP config generation — ported from the legacy in-VM Go gateway (mcp/config_gen.go).
//!
//! Generates per-agent MCP server configuration files that are mounted
//! read-only into the guest VM. Each agent type uses a different config format:
//!
//! | Agent   | Format   | Path                          |
//! |---------|----------|-------------------------------|
//! | Claude  | JSON     | ~/.claude/settings.json       |
//! | Codex   | TOML     | ~/.codex/mcp_servers.toml     |
//! | Goose   | YAML     | ~/.config/goose/config.yaml   |
//! | Cursor  | JSON     | ~/.cursor/mcp.json            |

use std::collections::HashMap;
use crate::config::{AgentType, McpServerConfig};

/// Format identifier for MCP config generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpConfigFormat {
    Claude,
    Codex,
    Goose,
    Cursor,
}

impl From<AgentType> for McpConfigFormat {
    fn from(at: AgentType) -> Self {
        match at {
            AgentType::Claude => McpConfigFormat::Claude,
            AgentType::Codex => McpConfigFormat::Codex,
            AgentType::Goose => McpConfigFormat::Goose,
            AgentType::Cursor => McpConfigFormat::Cursor,
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
                if let Some(content) = Self::generate_claude(servers) {
                    files.push(ConfigFile {
                        relative_path: "claude/settings.json".to_string(),
                        content,
                    });
                }
            }
            McpConfigFormat::Codex => {
                if let Some(content) = Self::generate_codex(servers) {
                    files.push(ConfigFile {
                        relative_path: "codex/mcp_servers.toml".to_string(),
                        content,
                    });
                }
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
        }

        files
    }

    /// Generate Claude Code MCP config (JSON, `mcpServers` key in settings.json).
    ///
    /// Format:
    /// ```json
    /// {
    ///   "mcpServers": {
    ///     "github": {
    ///       "command": "npx",
    ///       "args": ["-y", "@modelcontextprotocol/server-github"],
    ///       "env": { "GITHUB_TOKEN": "..." }
    ///     }
    ///   }
    /// }
    /// ```
    fn generate_claude(servers: &HashMap<String, McpServerConfig>) -> Option<Vec<u8>> {
        if servers.is_empty() {
            return None;
        }

        let mut entries = serde_json::Map::new();
        let mut names: Vec<&String> = servers.keys().collect();
        names.sort();

        for name in names {
            let srv = &servers[name];
            let mut entry = serde_json::Map::new();
            entry.insert("command".to_string(), serde_json::Value::String(srv.command.clone()));
            entry.insert(
                "args".to_string(),
                serde_json::Value::Array(srv.args.iter().map(|a| serde_json::Value::String(a.clone())).collect()),
            );
            if !srv.env.is_empty() {
                let env_map: serde_json::Map<String, _> = srv
                    .env
                    .iter()
                    .filter(|(_, v)| !v.is_empty())
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                    .collect();
                if !env_map.is_empty() {
                    entry.insert("env".to_string(), serde_json::Value::Object(env_map));
                }
            }
            entries.insert(name.clone(), serde_json::Value::Object(entry));
        }

        let wrapper = serde_json::json!({ "mcpServers": serde_json::Value::Object(entries) });
        let content = serde_json::to_string_pretty(&wrapper).ok()?;
        Some(content.into_bytes())
    }

    /// Generate Codex MCP config (TOML).
    ///
    /// Format:
    /// ```toml
    /// [mcp_servers.github]
    /// command = "npx"
    /// args = ["-y", "@modelcontextprotocol/server-github"]
    /// env = { GITHUB_TOKEN = "..." }
    /// ```
    fn generate_codex(servers: &HashMap<String, McpServerConfig>) -> Option<Vec<u8>> {
        if servers.is_empty() {
            return None;
        }

        let mut output = String::new();
        let mut names: Vec<&String> = servers.keys().collect();
        names.sort();

        for (i, name) in names.iter().enumerate() {
            let srv = &servers[*name];
            if i > 0 {
                output.push('\n');
            }
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

            let non_empty_env: HashMap<_, _> = srv.env.iter().filter(|(_, v)| !v.is_empty()).collect();
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

        Some(output.into_bytes())
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
        if servers.is_empty() {
            return None;
        }

        let mut output = String::new();
        output.push_str("GOOSE_TELEMETRY_ENABLED: false\n\n");
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

            let non_empty_env: HashMap<_, _> = srv.env.iter().filter(|(_, v)| !v.is_empty()).collect();
            if !non_empty_env.is_empty() {
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

    /// Generate Cursor MCP config (same JSON format as Claude).
    fn generate_cursor(servers: &HashMap<String, McpServerConfig>) -> Option<Vec<u8>> {
        Self::generate_claude(servers)
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
                args: vec!["-y".to_string(), "@modelcontextprotocol/server-github".to_string()],
                env: [("GITHUB_TOKEN".to_string(), "test-token".to_string())].into(),
                enabled: true,
            },
        );
        m.insert(
            "filesystem".to_string(),
            McpServerConfig {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "@modelcontextprotocol/server-filesystem".to_string()],
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
        assert_eq!(files[0].relative_path, "claude/settings.json");

        let content = String::from_utf8(files[0].content.clone()).unwrap();
        assert!(content.contains("mcpServers"));
        assert!(content.contains("github"));
        assert!(content.contains("npx"));
        assert!(content.contains("GITHUB_TOKEN"));
        assert!(content.contains("filesystem"));
    }

    #[test]
    fn test_generate_codex() {
        let servers = sample_servers();
        let files = ConfigGenerator::generate_all(&servers, &AgentType::Codex);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "codex/mcp_servers.toml");

        let content = String::from_utf8(files[0].content.clone()).unwrap();
        assert!(content.contains("[mcp_servers.github]"));
        assert!(content.contains("command = \"npx\""));
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
    }

    #[test]
    fn test_generate_empty_servers() {
        let servers = HashMap::new();
        let files = ConfigGenerator::generate_all(&servers, &AgentType::Claude);
        assert!(files.is_empty(), "empty servers should produce no files");
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
        assert!(!content.contains("envs:"), "empty env should not produce envs section");
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
}
