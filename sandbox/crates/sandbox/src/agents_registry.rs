//! Agents Registry Client
//!
//! Resolves agent definitions, skills, and MCP references from a local
//! agents-registry directory (cloned or shipped as a path).

use crate::config::file::expand_env_vars;
use crate::config::{AgentDefinition, AgentMcpRef, McpServerConfig, ResolvedAgentConfig, SkillDef};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{debug, warn};

const DEFAULT_PUBLIC_SKILLS_CATALOG_URL: &str =
    "https://raw.githubusercontent.com/nanosandboxai/nanosandbox.ai/main/src/data/skills-public.json";
const DEFAULT_REMOTE_LOCAL_SKILLS_CATALOG_URL: &str =
    "https://raw.githubusercontent.com/nanosandboxai/nanosandbox.ai/main/src/data/skills-local.json";
const DEFAULT_LOCAL_SKILLS_RAW_BASE_URL: &str =
    "https://raw.githubusercontent.com/nanosandboxai/agents-registry/main";
const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 10;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillSource {
    Local,
    Public,
}

#[derive(Debug, Clone)]
struct SkillCatalogEntry {
    name: String,
    source: SkillSource,
    description: String,
    tags: Vec<String>,
    local_path: Option<String>,
    source_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PublicSkillCatalogEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default, rename = "sourceUrl")]
    source_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RemoteLocalSkillCatalogEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    path: Option<String>,
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
    base_path: Option<PathBuf>,
    index: RegistryIndex,
    public_skills_catalog_url: String,
    remote_local_skills_catalog_url: String,
    local_skills_raw_base_url: String,
    http_timeout_secs: u64,
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
            base_path: Some(path.to_path_buf()),
            index,
            public_skills_catalog_url: std::env::var("NANOSB_SKILLS_PUBLIC_CATALOG_URL")
                .unwrap_or_else(|_| DEFAULT_PUBLIC_SKILLS_CATALOG_URL.to_string()),
            remote_local_skills_catalog_url: std::env::var("NANOSB_SKILLS_LOCAL_CATALOG_URL")
                .unwrap_or_else(|_| DEFAULT_REMOTE_LOCAL_SKILLS_CATALOG_URL.to_string()),
            local_skills_raw_base_url: std::env::var("NANOSB_LOCAL_SKILLS_RAW_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_LOCAL_SKILLS_RAW_BASE_URL.to_string()),
            http_timeout_secs: std::env::var("NANOSB_SKILLS_HTTP_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(DEFAULT_HTTP_TIMEOUT_SECS),
        })
    }

    /// Create a client with no local registry and online metadata enabled.
    pub fn online_only() -> Self {
        Self {
            base_path: None,
            index: RegistryIndex {
                agents: Vec::new(),
                skills: Vec::new(),
            },
            public_skills_catalog_url: std::env::var("NANOSB_SKILLS_PUBLIC_CATALOG_URL")
                .unwrap_or_else(|_| DEFAULT_PUBLIC_SKILLS_CATALOG_URL.to_string()),
            remote_local_skills_catalog_url: std::env::var("NANOSB_SKILLS_LOCAL_CATALOG_URL")
                .unwrap_or_else(|_| DEFAULT_REMOTE_LOCAL_SKILLS_CATALOG_URL.to_string()),
            local_skills_raw_base_url: std::env::var("NANOSB_LOCAL_SKILLS_RAW_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_LOCAL_SKILLS_RAW_BASE_URL.to_string()),
            http_timeout_secs: std::env::var("NANOSB_SKILLS_HTTP_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(DEFAULT_HTTP_TIMEOUT_SECS),
        }
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

        let base_path = self
            .base_path
            .as_ref()
            .ok_or_else(|| "No local agents registry available for agent resolution".to_string())?;
        let yaml_path = base_path.join(&entry.path);
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

    /// Resolve a skill by name using skill catalog metadata.
    ///
    /// Resolution is deterministic:
    /// - Match by name in merged metadata.
    /// - Prefer local source over public when duplicates exist.
    /// - If name is absent from metadata, return unknown skill.
    pub fn resolve_skill(&self, name: &str) -> Result<SkillDef, String> {
        if let Some(local_entry) = self.local_catalog_entry_for(name) {
            return self.resolve_local_skill_from_catalog_entry(&local_entry);
        }

        let remote_catalog = self.load_remote_skill_catalog();
        let entry = select_skill_entry(name, &remote_catalog).ok_or_else(|| {
            format!(
                "Unknown skill '{}': not found in skill metadata catalog",
                name
            )
        })?;

        match entry.source {
            SkillSource::Local => self.resolve_local_skill_from_catalog_entry(&entry),
            SkillSource::Public => self.resolve_public_skill_from_catalog_entry(&entry),
        }
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
            interactive: false,
            permissions: crate::config::Permissions::Default,
            agent_type: None,
            claude_settings: None,
        })
    }

    /// Resolve a list of skills with no agent definition.
    ///
    /// Returns a `ResolvedAgentConfig` with only the given skills populated
    /// and all other fields at their defaults.
    pub fn resolve_skills_only(&self, skills: &[String]) -> Result<ResolvedAgentConfig, String> {
        let mut resolved_skills = Vec::new();
        for name in skills {
            match self.resolve_skill(name) {
                Ok(skill) => resolved_skills.push(skill),
                Err(e) => warn!("Failed to resolve skill '{}': {}", name, e),
            }
        }
        Ok(ResolvedAgentConfig {
            agent_name: String::new(),
            prompt: String::new(),
            skills: resolved_skills,
            mcp_servers: std::collections::HashMap::new(),
            auto_mode: false,
            interactive: false,
            permissions: crate::config::Permissions::Default,
            agent_type: None,
            claude_settings: None,
        })
    }

    fn local_catalog_entry_for(&self, name: &str) -> Option<SkillCatalogEntry> {
        self.index
            .skills
            .iter()
            .find(|skill| skill.name == name)
            .map(|skill| SkillCatalogEntry {
                name: skill.name.clone(),
                source: SkillSource::Local,
                description: skill.description.clone(),
                tags: skill.tags.clone(),
                local_path: Some(skill.path.clone()),
                source_url: None,
            })
    }

    fn load_remote_skill_catalog(&self) -> Vec<SkillCatalogEntry> {
        let mut entries = Vec::new();

        let remote_local_raw = match fetch_text(
            &self.remote_local_skills_catalog_url,
            self.http_timeout_secs,
        ) {
            Ok(raw) => raw,
            Err(e) => {
                warn!(
                    "Failed to fetch remote local skills catalog {}: {}",
                    self.remote_local_skills_catalog_url, e
                );
                String::new()
            }
        };
        if !remote_local_raw.is_empty() {
            match serde_json::from_str::<Vec<RemoteLocalSkillCatalogEntry>>(&remote_local_raw) {
                Ok(remote_local_entries) => {
                    for skill in remote_local_entries {
                        if skill.name.trim().is_empty() {
                            continue;
                        }
                        entries.push(SkillCatalogEntry {
                            name: skill.name,
                            source: SkillSource::Local,
                            description: skill.description,
                            tags: skill.tags,
                            local_path: skill.path,
                            source_url: None,
                        });
                    }
                }
                Err(e) => warn!(
                    "Failed to parse remote local skills catalog {}: {}",
                    self.remote_local_skills_catalog_url, e
                ),
            }
        }

        let public_raw = match fetch_text(&self.public_skills_catalog_url, self.http_timeout_secs) {
            Ok(raw) => raw,
            Err(e) => {
                warn!(
                    "Failed to fetch public skills catalog {}: {}",
                    self.public_skills_catalog_url, e
                );
                String::new()
            }
        };
        if !public_raw.is_empty() {
            match serde_json::from_str::<Vec<PublicSkillCatalogEntry>>(&public_raw) {
                Ok(public_entries) => {
                    for skill in public_entries {
                        if skill.name.trim().is_empty() {
                            continue;
                        }
                        entries.push(SkillCatalogEntry {
                            name: skill.name,
                            source: SkillSource::Public,
                            description: skill.description,
                            tags: skill.tags,
                            local_path: None,
                            source_url: skill.source_url,
                        });
                    }
                }
                Err(e) => warn!(
                    "Failed to parse public skills catalog {}: {}",
                    self.public_skills_catalog_url, e
                ),
            }
        }

        entries
    }

    fn resolve_local_skill_from_catalog_entry(
        &self,
        entry: &SkillCatalogEntry,
    ) -> Result<SkillDef, String> {
        let local_path = entry
            .local_path
            .as_deref()
            .ok_or_else(|| format!("Local skill '{}' missing path in metadata", entry.name))?;

        if let Some(base_path) = &self.base_path {
            let md_path = base_path.join(local_path);
            if md_path.exists() {
                let content = std::fs::read_to_string(&md_path).map_err(|e| {
                    format!("Failed to read skill file {}: {}", md_path.display(), e)
                })?;
                return parse_skill_markdown(&entry.name, &content);
            }
        }

        let raw_url = format!(
            "{}/{}",
            self.local_skills_raw_base_url.trim_end_matches('/'),
            local_path.trim_start_matches('/')
        );
        let content = fetch_text(&raw_url, self.http_timeout_secs).map_err(|e| {
            format!(
                "Failed to fetch local skill '{}' from {}: {}",
                entry.name, raw_url, e
            )
        })?;
        parse_skill_markdown(&entry.name, &content)
    }

    fn resolve_public_skill_from_catalog_entry(
        &self,
        entry: &SkillCatalogEntry,
    ) -> Result<SkillDef, String> {
        let source_url = entry.source_url.as_deref().ok_or_else(|| {
            format!(
                "Public skill '{}' missing sourceUrl in metadata",
                entry.name
            )
        })?;

        let content = fetch_public_skill_markdown(source_url, &entry.name, self.http_timeout_secs)
            .map_err(|e| {
                format!(
                    "Failed to resolve public skill '{}' from {}: {}",
                    entry.name, source_url, e
                )
            })?;
        parse_skill_markdown(&entry.name, &content)
    }
}

fn select_skill_entry(name: &str, entries: &[SkillCatalogEntry]) -> Option<SkillCatalogEntry> {
    let mut matches: Vec<SkillCatalogEntry> = entries
        .iter()
        .filter(|entry| entry.name == name)
        .cloned()
        .collect();
    if matches.is_empty() {
        return None;
    }

    matches.sort_by(|a, b| {
        let rank_a = skill_source_rank(a.source);
        let rank_b = skill_source_rank(b.source);
        rank_a
            .cmp(&rank_b)
            .then_with(|| b.description.len().cmp(&a.description.len()))
            .then_with(|| b.tags.len().cmp(&a.tags.len()))
    });

    matches.into_iter().next()
}

fn skill_source_rank(source: SkillSource) -> u8 {
    match source {
        SkillSource::Local => 0,
        SkillSource::Public => 1,
    }
}

fn fetch_text(url: &str, timeout_secs: u64) -> Result<String, String> {
    let timeout = Duration::from_secs(timeout_secs.max(1));
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .call()
        .map_err(|e| format!("HTTP GET failed for {}: {}", url, e))?;

    response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Failed to read response body for {}: {}", url, e))
}

fn fetch_public_skill_markdown(
    source_url: &str,
    skill_name: &str,
    timeout_secs: u64,
) -> Result<String, String> {
    if source_url.contains("raw.githubusercontent.com") && source_url.ends_with(".md") {
        return fetch_text(source_url, timeout_secs);
    }

    let mut candidates: Vec<String> = Vec::new();

    if let Some(base) = github_tree_or_blob_to_raw_base(source_url) {
        candidates.push(format!("{}/SKILL.md", base));
        candidates.push(format!("{}/README.md", base));
        candidates.push(format!("{}/{}.md", base, skill_name));
        candidates.push(format!("{}/{}.md", base, skill_name.replace('-', "_")));
    } else if let Some(base) = github_repo_to_raw_base(source_url) {
        candidates.push(format!("{}/SKILL.md", base));
        candidates.push(format!("{}/README.md", base));
        candidates.push(format!("{}/{}.md", base, skill_name));
    }

    for candidate in candidates {
        match fetch_text(&candidate, timeout_secs) {
            Ok(content) => return Ok(content),
            Err(_) => continue,
        }
    }

    Err("No supported markdown entrypoint found for public skill source".to_string())
}

fn github_tree_or_blob_to_raw_base(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let parts: Vec<&str> = trimmed.split('/').collect();
    if parts.len() < 7 {
        return None;
    }
    if parts[2] != "github.com" {
        return None;
    }
    if parts[5] != "tree" && parts[5] != "blob" {
        return None;
    }
    let owner = parts[3];
    let repo = parts[4];
    let branch = parts[6];
    let path = if parts.len() > 7 {
        format!("/{}", parts[7..].join("/"))
    } else {
        String::new()
    };

    Some(format!(
        "https://raw.githubusercontent.com/{}/{}/{}{}",
        owner, repo, branch, path
    ))
}

fn github_repo_to_raw_base(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let parts: Vec<&str> = trimmed.split('/').collect();
    if parts.len() < 5 {
        return None;
    }
    if parts[2] != "github.com" {
        return None;
    }
    let owner = parts[3];
    let repo = parts[4];
    Some(format!(
        "https://raw.githubusercontent.com/{}/{}/main",
        owner, repo
    ))
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
            when_to_use: String::new(),
            allowed_tools: Vec::new(),
            user_invocable: None,
            paths: Vec::new(),
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
        #[serde(default)]
        when_to_use: Option<String>,
        #[serde(default)]
        allowed_tools: Vec<String>,
        #[serde(default)]
        user_invocable: Option<bool>,
        #[serde(default)]
        paths: Vec<String>,
    }

    let fm: SkillFrontmatter = serde_yaml::from_str(frontmatter)
        .map_err(|e| format!("Failed to parse skill frontmatter: {}", e))?;

    Ok(SkillDef {
        name: fm.name.unwrap_or_else(|| fallback_name.to_string()),
        description: fm.description.unwrap_or_default(),
        content: body.to_string(),
        version: fm.version.unwrap_or_default(),
        tags: fm.tags,
        when_to_use: fm.when_to_use.unwrap_or_default(),
        allowed_tools: fm.allowed_tools,
        user_invocable: fm.user_invocable,
        paths: fm.paths,
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
        assert!(result
            .unwrap_err()
            .contains("not found in skill metadata catalog"));
    }

    #[test]
    fn test_select_skill_entry_prefers_local_source() {
        let entries = vec![
            SkillCatalogEntry {
                name: "tdd".to_string(),
                source: SkillSource::Public,
                description: "public".to_string(),
                tags: vec!["public".to_string()],
                local_path: None,
                source_url: Some("https://example.com/public".to_string()),
            },
            SkillCatalogEntry {
                name: "tdd".to_string(),
                source: SkillSource::Local,
                description: "local".to_string(),
                tags: vec!["local".to_string()],
                local_path: Some("skills/tdd.md".to_string()),
                source_url: None,
            },
        ];

        let selected = select_skill_entry("tdd", &entries).unwrap();
        assert_eq!(selected.source, SkillSource::Local);
    }

    #[test]
    fn test_github_tree_to_raw_base() {
        let url = "https://github.com/anthropics/skills/tree/main/skills/frontend-design";
        let raw = github_tree_or_blob_to_raw_base(url).unwrap();
        assert_eq!(
            raw,
            "https://raw.githubusercontent.com/anthropics/skills/main/skills/frontend-design"
        );
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
        assert!(github
            .args
            .contains(&"@modelcontextprotocol/server-github".to_string()));
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
        assert_eq!(
            config.args,
            vec!["-y", "@modelcontextprotocol/server-github"]
        );
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
            when_to_use: String::new(),
            allowed_tools: Vec::new(),
            user_invocable: None,
            paths: Vec::new(),
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
                when_to_use: String::new(),
                allowed_tools: Vec::new(),
                user_invocable: None,
                paths: Vec::new(),
            }],
            mcp_servers: HashMap::new(),
            auto_mode: false,
            interactive: false,
            permissions: crate::config::Permissions::Default,
            agent_type: None,
            claude_settings: None,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: ResolvedAgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent_name, "python-developer");
        assert_eq!(parsed.skills.len(), 1);
    }
}
