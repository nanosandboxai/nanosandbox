//! Agents Registry Client
//!
//! Resolves agent definitions, skills, and MCP references from a local
//! agents-registry directory (cloned or shipped as a path).

use crate::config::{
    AgentDefinition, AgentMcpRef, McpServerConfig, ResolvedAgentConfig, SkillDef,
};
use crate::config::file::expand_env_vars;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Registry index (parsed from index.json).
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryIndex {
    #[serde(default)]
    pub agents: Vec<RegistryAgentEntry>,
    #[serde(default)]
    pub skills: Vec<RegistrySkillEntry>,
}

/// Agent entry in the registry index.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryAgentEntry {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub mcps: Vec<String>,
    pub path: String,
}

/// Skill entry in the registry index.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistrySkillEntry {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub path: String,
}

/// Raw agent YAML file structure.
#[derive(Debug, Deserialize)]
struct AgentYaml {
    metadata: AgentYamlMetadata,
    spec: AgentYamlSpec,
}

#[derive(Debug, Deserialize)]
struct AgentYamlMetadata {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AgentYamlSpec {
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    mcps: Vec<AgentYamlMcp>,
}

#[derive(Debug, Deserialize)]
struct AgentYamlMcp {
    name: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    package: Option<String>,
    #[serde(default)]
    env: HashMap<String, String>,
}

/// Client for reading agent definitions and skills from a local registry directory.
#[derive(Debug, Clone)]
pub struct AgentsRegistryClient {
    base_path: PathBuf,
    index: RegistryIndex,
}

impl AgentsRegistryClient {
    /// Create a client from a local filesystem path (cloned registry).
    pub fn from_path(path: &Path) -> Result<Self, String> {
        let index_path = path.join("index.json");
        if !index_path.exists() {
            return Err(format!(
                "Registry index not found at {}",
                index_path.display()
            ));
        }

        let content = std::fs::read_to_string(&index_path)
            .map_err(|e| format!("Failed to read registry index: {}", e))?;
        let index: RegistryIndex = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse registry index: {}", e))?;

        debug!(
            "Loaded agents registry: {} agents, {} skills",
            index.agents.len(),
            index.skills.len()
        );

        Ok(Self {
            base_path: path.to_path_buf(),
            index,
        })
    }

    /// List all available agent names.
    pub fn list_agents(&self) -> Vec<&RegistryAgentEntry> {
        self.index.agents.iter().collect()
    }

    /// List all available skill names.
    pub fn list_skills(&self) -> Vec<&RegistrySkillEntry> {
        self.index.skills.iter().collect()
    }

    /// Resolve an agent definition by name.
    pub fn resolve_agent(&self, name: &str) -> Result<AgentDefinition, String> {
        let entry = self
            .index
            .agents
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(|| format!("Agent '{}' not found in registry", name))?;

        let yaml_path = self.base_path.join(&entry.path);
        let content = std::fs::read_to_string(&yaml_path)
            .map_err(|e| format!("Failed to read agent file {}: {}", yaml_path.display(), e))?;

        let agent_yaml: AgentYaml = serde_yaml::from_str(&content)
            .map_err(|e| format!("Failed to parse agent YAML {}: {}", entry.path, e))?;

        Ok(AgentDefinition {
            name: agent_yaml.metadata.name,
            description: agent_yaml.metadata.description,
            prompt: agent_yaml.spec.prompt.trim().to_string(),
            skills: agent_yaml.spec.skills,
            mcps: agent_yaml
                .spec
                .mcps
                .into_iter()
                .map(|m| AgentMcpRef {
                    name: m.name,
                    source: m.source,
                    package: m.package,
                    env: m.env,
                })
                .collect(),
            tags: agent_yaml.metadata.tags,
        })
    }

    /// Resolve a skill by name from the registry.
    pub fn resolve_skill(&self, name: &str) -> Result<SkillDef, String> {
        let entry = self
            .index
            .skills
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("Skill '{}' not found in registry", name))?;

        let md_path = self.base_path.join(&entry.path);
        let content = std::fs::read_to_string(&md_path)
            .map_err(|e| format!("Failed to read skill file {}: {}", md_path.display(), e))?;

        parse_skill_markdown(name, &content)
    }

    /// Resolve a full agent config: agent definition + all skills + MCPs.
    ///
    /// `extra_skills` are additional skill names to include beyond what the
    /// agent definition specifies.
    pub fn resolve_full(
        &self,
        agent_name: &str,
        extra_skills: &[String],
    ) -> Result<ResolvedAgentConfig, String> {
        let agent = self.resolve_agent(agent_name)?;

        // Collect unique skill names (agent's skills + extras)
        let mut skill_names: Vec<String> = agent.skills.clone();
        for s in extra_skills {
            if !skill_names.contains(s) {
                skill_names.push(s.clone());
            }
        }

        // Resolve all skills
        let mut skills = Vec::new();
        for name in &skill_names {
            match self.resolve_skill(name) {
                Ok(skill) => skills.push(skill),
                Err(e) => warn!("Failed to resolve skill '{}': {}", name, e),
            }
        }

        // Resolve MCPs into McpServerConfig
        let mut mcp_servers = HashMap::new();
        for mcp_ref in &agent.mcps {
            match resolve_mcp_ref(mcp_ref) {
                Ok(config) => {
                    mcp_servers.insert(mcp_ref.name.clone(), config);
                }
                Err(e) => warn!("Failed to resolve MCP '{}': {}", mcp_ref.name, e),
            }
        }

        Ok(ResolvedAgentConfig {
            agent_name: agent.name,
            prompt: agent.prompt,
            skills,
            mcp_servers,
            auto_mode: false,
        })
    }
}

/// Parse a skill markdown file with YAML frontmatter.
///
/// Format:
/// ```text
/// ---
/// name: tdd
/// description: Test-driven development
/// version: "1.0"
/// tags: [testing, tdd]
/// ---
///
/// # Markdown content here
/// ```
fn parse_skill_markdown(fallback_name: &str, content: &str) -> Result<SkillDef, String> {
    let trimmed = content.trim();
    if !trimmed.starts_with("---") {
        // No frontmatter — treat entire content as the skill body
        return Ok(SkillDef {
            name: fallback_name.to_string(),
            description: String::new(),
            content: content.to_string(),
            version: String::new(),
            tags: Vec::new(),
        });
    }

    // Find the closing ---
    let after_first = &trimmed[3..];
    let end_idx = after_first
        .find("\n---")
        .ok_or("Skill markdown has opening --- but no closing ---")?;

    let frontmatter = &after_first[..end_idx];
    let body = after_first[end_idx + 4..].trim_start();

    #[derive(Deserialize)]
    struct SkillFrontmatter {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        version: Option<String>,
        #[serde(default)]
        tags: Vec<String>,
    }

    let fm: SkillFrontmatter = serde_yaml::from_str(frontmatter)
        .map_err(|e| format!("Failed to parse skill frontmatter: {}", e))?;

    Ok(SkillDef {
        name: fm.name.unwrap_or_else(|| fallback_name.to_string()),
        description: fm.description.unwrap_or_default(),
        content: body.to_string(),
        version: fm.version.unwrap_or_default(),
        tags: fm.tags,
    })
}

/// Resolve an AgentMcpRef into a McpServerConfig.
fn resolve_mcp_ref(mcp_ref: &AgentMcpRef) -> Result<McpServerConfig, String> {
    let package = mcp_ref
        .package
        .as_ref()
        .ok_or_else(|| format!("MCP '{}' has no package specified", mcp_ref.name))?;

    // Determine runner command based on source/package
    let (command, args) = if package.starts_with('@') || package.contains('/') {
        // NPM package — use npx
        ("npx".to_string(), vec!["-y".to_string(), package.clone()])
    } else {
        // Python package — use uvx
        ("uvx".to_string(), vec![package.clone()])
    };

    // Expand env vars
    let mut env = HashMap::new();
    for (k, v) in &mcp_ref.env {
        match expand_env_vars(v) {
            Ok(expanded) => {
                env.insert(k.clone(), expanded);
            }
            Err(e) => {
                warn!(
                    "MCP '{}' env var '{}' expansion failed: {}",
                    mcp_ref.name, k, e
                );
            }
        }
    }

    Ok(McpServerConfig {
        command,
        args,
        env,
        enabled: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_registry() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let base = dir.path().to_path_buf();

        // index.json
        let index = r#"{
            "version": "1.0",
            "agents": [
                {
                    "name": "python-developer",
                    "description": "Python dev",
                    "tags": ["python"],
                    "skills": ["tdd", "git-workflow"],
                    "mcps": ["server-github"],
                    "path": "agents/python-developer.yaml"
                }
            ],
            "skills": [
                {
                    "name": "tdd",
                    "description": "TDD workflow",
                    "tags": ["testing"],
                    "path": "skills/tdd.md"
                },
                {
                    "name": "git-workflow",
                    "description": "Git workflow",
                    "tags": ["git"],
                    "path": "skills/git-workflow.md"
                }
            ]
        }"#;
        fs::write(base.join("index.json"), index).unwrap();

        // agents/
        fs::create_dir_all(base.join("agents")).unwrap();
        let agent_yaml = r#"
apiVersion: v1
kind: Agent
metadata:
  name: python-developer
  description: Senior Python developer
  tags: [python, backend]

spec:
  prompt: |
    You are a senior Python developer.

  skills:
    - tdd
    - git-workflow

  mcps:
    - name: server-github
      source: smithery
      package: "@modelcontextprotocol/server-github"
      env:
        GITHUB_PERSONAL_ACCESS_TOKEN: "test-token"
"#;
        fs::write(base.join("agents/python-developer.yaml"), agent_yaml).unwrap();

        // skills/
        fs::create_dir_all(base.join("skills")).unwrap();
        let tdd_md = r#"---
name: tdd
description: Test-driven development workflow
version: "1.0"
tags: [testing, tdd]
---

# Test-Driven Development

Follow the Red-Green-Refactor cycle.
"#;
        fs::write(base.join("skills/tdd.md"), tdd_md).unwrap();

        let git_md = r#"---
name: git-workflow
description: Git workflow best practices
version: "1.0"
tags: [git]
---

# Git Workflow

Use conventional commits.
"#;
        fs::write(base.join("skills/git-workflow.md"), git_md).unwrap();

        (dir, base)
    }

    #[test]
    fn test_from_path() {
        let (_dir, base) = create_test_registry();
        let client = AgentsRegistryClient::from_path(&base).unwrap();
        assert_eq!(client.list_agents().len(), 1);
        assert_eq!(client.list_skills().len(), 2);
    }

    #[test]
    fn test_from_path_missing_index() {
        let dir = TempDir::new().unwrap();
        let result = AgentsRegistryClient::from_path(dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("index not found"));
    }

    #[test]
    fn test_resolve_agent() {
        let (_dir, base) = create_test_registry();
        let client = AgentsRegistryClient::from_path(&base).unwrap();
        let agent = client.resolve_agent("python-developer").unwrap();

        assert_eq!(agent.name, "python-developer");
        assert!(agent.prompt.contains("senior Python developer"));
        assert_eq!(agent.skills, vec!["tdd", "git-workflow"]);
        assert_eq!(agent.mcps.len(), 1);
        assert_eq!(agent.mcps[0].name, "server-github");
    }

    #[test]
    fn test_resolve_agent_not_found() {
        let (_dir, base) = create_test_registry();
        let client = AgentsRegistryClient::from_path(&base).unwrap();
        let result = client.resolve_agent("nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_resolve_skill() {
        let (_dir, base) = create_test_registry();
        let client = AgentsRegistryClient::from_path(&base).unwrap();
        let skill = client.resolve_skill("tdd").unwrap();

        assert_eq!(skill.name, "tdd");
        assert_eq!(skill.description, "Test-driven development workflow");
        assert_eq!(skill.version, "1.0");
        assert!(skill.content.contains("Red-Green-Refactor"));
        assert!(skill.tags.contains(&"testing".to_string()));
    }

    #[test]
    fn test_resolve_skill_not_found() {
        let (_dir, base) = create_test_registry();
        let client = AgentsRegistryClient::from_path(&base).unwrap();
        let result = client.resolve_skill("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_full() {
        let (_dir, base) = create_test_registry();
        let client = AgentsRegistryClient::from_path(&base).unwrap();
        let resolved = client.resolve_full("python-developer", &[]).unwrap();

        assert_eq!(resolved.agent_name, "python-developer");
        assert!(resolved.prompt.contains("senior Python developer"));
        assert_eq!(resolved.skills.len(), 2);
        assert!(resolved.mcp_servers.contains_key("server-github"));

        let github = &resolved.mcp_servers["server-github"];
        assert_eq!(github.command, "npx");
        assert!(github.args.contains(&"@modelcontextprotocol/server-github".to_string()));
    }

    #[test]
    fn test_resolve_full_with_extra_skills() {
        let (_dir, base) = create_test_registry();
        let client = AgentsRegistryClient::from_path(&base).unwrap();
        // "tdd" is already in the agent's skills, "git-workflow" is also already there
        // Adding an extra duplicate should not double-add
        let resolved = client
            .resolve_full("python-developer", &["tdd".to_string()])
            .unwrap();
        assert_eq!(resolved.skills.len(), 2);
    }

    #[test]
    fn test_parse_skill_markdown_with_frontmatter() {
        let content = r#"---
name: test-skill
description: A test skill
version: "2.0"
tags: [a, b]
---

# Content

Body here.
"#;
        let skill = parse_skill_markdown("fallback", content).unwrap();
        assert_eq!(skill.name, "test-skill");
        assert_eq!(skill.description, "A test skill");
        assert_eq!(skill.version, "2.0");
        assert!(skill.content.contains("Body here."));
    }

    #[test]
    fn test_parse_skill_markdown_no_frontmatter() {
        let content = "# Just markdown\n\nNo frontmatter here.";
        let skill = parse_skill_markdown("my-skill", content).unwrap();
        assert_eq!(skill.name, "my-skill");
        assert!(skill.content.contains("No frontmatter here."));
    }

    #[test]
    fn test_resolve_mcp_ref_npm() {
        let mcp_ref = AgentMcpRef {
            name: "github".to_string(),
            source: "smithery".to_string(),
            package: Some("@modelcontextprotocol/server-github".to_string()),
            env: HashMap::new(),
        };
        let config = resolve_mcp_ref(&mcp_ref).unwrap();
        assert_eq!(config.command, "npx");
        assert_eq!(config.args, vec!["-y", "@modelcontextprotocol/server-github"]);
        assert!(config.enabled);
    }

    #[test]
    fn test_resolve_mcp_ref_python() {
        let mcp_ref = AgentMcpRef {
            name: "fetch".to_string(),
            source: "official".to_string(),
            package: Some("mcp-server-fetch".to_string()),
            env: HashMap::new(),
        };
        let config = resolve_mcp_ref(&mcp_ref).unwrap();
        assert_eq!(config.command, "uvx");
        assert_eq!(config.args, vec!["mcp-server-fetch"]);
    }

    #[test]
    fn test_resolve_mcp_ref_no_package() {
        let mcp_ref = AgentMcpRef {
            name: "test".to_string(),
            source: "custom".to_string(),
            package: None,
            env: HashMap::new(),
        };
        let result = resolve_mcp_ref(&mcp_ref);
        assert!(result.is_err());
    }

    #[test]
    fn test_skill_def_serialization() {
        let skill = SkillDef {
            name: "tdd".to_string(),
            description: "Test-driven development".to_string(),
            content: "# TDD\n\nRed-green-refactor.".to_string(),
            version: "1.0".to_string(),
            tags: vec!["testing".to_string()],
        };
        let json = serde_json::to_string(&skill).unwrap();
        let parsed: SkillDef = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "tdd");
        assert_eq!(parsed.content, "# TDD\n\nRed-green-refactor.");
    }

    #[test]
    fn test_resolved_agent_config_serialization() {
        let config = ResolvedAgentConfig {
            agent_name: "python-developer".to_string(),
            prompt: "You are a Python developer.".to_string(),
            skills: vec![SkillDef {
                name: "tdd".to_string(),
                description: "TDD".to_string(),
                content: "# TDD".to_string(),
                version: "1.0".to_string(),
                tags: vec![],
            }],
            mcp_servers: HashMap::new(),
            auto_mode: false,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: ResolvedAgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent_name, "python-developer");
        assert_eq!(parsed.skills.len(), 1);
    }
}
