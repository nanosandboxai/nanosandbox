//! Skills config generation — ported from the legacy in-VM Go gateway (skills/config_gen.go).
//!
//! Generates per-agent skill/prompt files that are mounted read-only into the
//! guest VM. Each agent type uses a different format:
//!
//! | Agent   | Skills format                          | Prompt file              |
//! |---------|----------------------------------------|--------------------------|
//! | Claude  | Individual SKILL.md files per skill    | CLAUDE.md                |
//! | Codex   | Individual SKILL.md files per skill    | AGENTS.md                |
//! | Goose   | Everything concatenated into one file  | .goosehints              |
//! | Cursor  | Individual .mdc rule files per skill   | nanosandbox-agent.mdc    |

use crate::config::{AgentType, SkillDef};
use super::config_gen::ConfigFile;

/// Skills config generator.
pub struct SkillsGenerator;

/// The sandbox preamble prepended to every agent prompt file.
const SANDBOX_PREAMBLE: &str = r#"## Sandbox Environment

You are running inside an isolated nanosandbox VM (Debian Linux). You have:
- Full sudo access (passwordless) — use it freely to install packages and tools.
- Node.js is pre-installed. Other runtimes (Python, Go, Rust, etc.) are NOT.
- When a task requires a runtime or tool that is missing, install it yourself
  using `sudo apt-get update && sudo apt-get install -y <package>` before proceeding.
  Do NOT ask the user to install software — you have full permissions to do it.
- The working directory is /workspace (project files are mounted here).
- Network access is available for downloading packages and dependencies.

"#;

impl SkillsGenerator {
    /// Generate all skills/prompt config files for a given agent type.
    ///
    /// * `skills` - Resolved skill definitions.
    /// * `agent_type` - The agent type.
    /// * `agent_name` - The agent definition name.
    /// * `prompt` - The agent system prompt.
    pub fn generate_all(
        skills: &[SkillDef],
        agent_type: &AgentType,
        agent_name: &str,
        prompt: &str,
    ) -> Vec<ConfigFile> {
        match agent_type {
            AgentType::Claude => Self::generate_claude(skills, agent_name, prompt),
            AgentType::Codex => Self::generate_codex(skills, agent_name, prompt),
            AgentType::Goose => Self::generate_goose(skills, agent_name, prompt),
            AgentType::Cursor => Self::generate_cursor(skills, agent_name, prompt),
        }
    }

    // ── Claude Code Format ──────────────────────────────────────────
    // Skills: ~/.claude/skills/<name>/SKILL.md
    // Prompt: ~/.claude/CLAUDE.md

    fn generate_claude(skills: &[SkillDef], _agent_name: &str, prompt: &str) -> Vec<ConfigFile> {
        let mut files = Vec::new();

        // Individual skill files
        for skill in skills {
            let content = Self::format_skill_md(skill);
            files.push(ConfigFile {
                relative_path: format!("claude/skills/{}/SKILL.md", skill.name),
                content: content.into_bytes(),
            });
        }

        // CLAUDE.md prompt
        let prompt_content = format!("{}{}\n", SANDBOX_PREAMBLE, prompt);
        files.push(ConfigFile {
            relative_path: "claude/CLAUDE.md".to_string(),
            content: prompt_content.into_bytes(),
        });

        files
    }

    // ── Goose Format ────────────────────────────────────────────────
    // Everything goes into .goosehints (no native skill files)

    fn generate_goose(skills: &[SkillDef], agent_name: &str, prompt: &str) -> Vec<ConfigFile> {
        let mut output = String::new();
        output.push_str(SANDBOX_PREAMBLE);

        if !agent_name.is_empty() || !prompt.is_empty() {
            output.push_str(prompt);
            output.push_str("\n\n");
        }

        for skill in skills {
            output.push_str(&format!("## {}\n\n", skill.name));
            if !skill.description.is_empty() {
                output.push_str(&skill.description);
                output.push_str("\n\n");
            }
            output.push_str(&skill.content);
            output.push_str("\n\n");
        }

        if output.is_empty() {
            return Vec::new();
        }

        vec![ConfigFile {
            relative_path: "goose/.goosehints".to_string(),
            content: output.into_bytes(),
        }]
    }

    // ── Codex Format ────────────────────────────────────────────────
    // Skills: ~/.agents/skills/<name>/SKILL.md
    // Prompt: ~/.codex/AGENTS.md

    fn generate_codex(skills: &[SkillDef], _agent_name: &str, prompt: &str) -> Vec<ConfigFile> {
        let mut files = Vec::new();

        // Individual skill files under .agents/skills/
        for skill in skills {
            let content = Self::format_skill_md(skill);
            files.push(ConfigFile {
                relative_path: format!("agents/skills/{}/SKILL.md", skill.name),
                content: content.into_bytes(),
            });
        }

        // AGENTS.md prompt
        let prompt_content = format!("{}{}\n", SANDBOX_PREAMBLE, prompt);
        files.push(ConfigFile {
            relative_path: "codex/AGENTS.md".to_string(),
            content: prompt_content.into_bytes(),
        });

        files
    }

    // ── Cursor Format ───────────────────────────────────────────────
    // Skills: ~/.cursor/rules/nanosb-<name>.mdc
    // Prompt: ~/.cursor/rules/nanosandbox-agent.mdc (alwaysApply)

    fn generate_cursor(skills: &[SkillDef], agent_name: &str, prompt: &str) -> Vec<ConfigFile> {
        let mut files = Vec::new();

        // Individual .mdc rule files
        for skill in skills {
            let mut content = String::new();
            content.push_str("---\n");
            content.push_str(&format!("description: \"{}\"\n", skill.description));
            content.push_str("alwaysApply: false\n");
            if !skill.paths.is_empty() {
                content.push_str(&format!("globs: {}\n", skill.paths.join(", ")));
            }
            content.push_str("---\n\n");
            content.push_str(&format!("# {}\n\n", skill.name));
            content.push_str(&skill.content);
            content.push('\n');

            files.push(ConfigFile {
                relative_path: format!("cursor/rules/nanosb-{}.mdc", skill.name),
                content: content.into_bytes(),
            });
        }

        // Always-apply preamble rule
        let mut preamble = String::new();
        preamble.push_str("---\n");
        preamble.push_str(&format!("description: \"Nanosandbox agent definition: {}\"\n", agent_name));
        preamble.push_str("alwaysApply: true\n");
        preamble.push_str("---\n\n");
        preamble.push_str(SANDBOX_PREAMBLE);
        preamble.push_str(prompt);
        preamble.push('\n');

        files.push(ConfigFile {
            relative_path: "cursor/rules/nanosandbox-agent.mdc".to_string(),
            content: preamble.into_bytes(),
        });

        files
    }

    // ── Helpers ─────────────────────────────────────────────────────

    /// Format a skill as a SKILL.md file with YAML frontmatter.
    fn format_skill_md(skill: &SkillDef) -> String {
        let mut b = String::new();
        b.push_str("---\n");
        b.push_str(&format!("name: {}\n", skill.name));
        b.push_str(&format!("description: {}\n", skill.description));
        if !skill.version.is_empty() {
            b.push_str(&format!("version: {:?}\n", skill.version));
        }
        if !skill.when_to_use.is_empty() {
            b.push_str(&format!("when_to_use: {:?}\n", skill.when_to_use));
        }
        if !skill.allowed_tools.is_empty() {
            b.push_str(&format!("allowed-tools: {}\n", skill.allowed_tools.join(" ")));
        }
        if let Some(false) = skill.user_invocable {
            b.push_str("user-invocable: false\n");
        }
        if !skill.paths.is_empty() {
            b.push_str("paths:\n");
            for p in &skill.paths {
                b.push_str(&format!("  - {:?}\n", p));
            }
        }
        b.push_str("---\n\n");
        b.push_str(&skill.content);
        b.push('\n');
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_skills() -> Vec<SkillDef> {
        vec![
            SkillDef {
                name: "tdd".to_string(),
                description: "Test-driven development".to_string(),
                content: "# TDD\n\nRed-green-refactor.".to_string(),
                version: "1.0".to_string(),
                tags: vec!["testing".to_string()],
                when_to_use: "When writing tests".to_string(),
                allowed_tools: Vec::new(),
                user_invocable: None,
                paths: Vec::new(),
            },
            SkillDef {
                name: "git-workflow".to_string(),
                description: "Git workflow".to_string(),
                content: "# Git\n\nUse conventional commits.".to_string(),
                version: String::new(),
                tags: vec!["git".to_string()],
                when_to_use: String::new(),
                allowed_tools: Vec::new(),
                user_invocable: None,
                paths: Vec::new(),
            },
        ]
    }

    #[test]
    fn test_generate_claude_skills() {
        let skills = sample_skills();
        let files = SkillsGenerator::generate_all(&skills, &AgentType::Claude, "python-dev", "You are a Python dev.");

        // 2 skill files + 1 CLAUDE.md
        assert_eq!(files.len(), 3);

        let claude_md = files.iter().find(|f| f.relative_path == "claude/CLAUDE.md");
        assert!(claude_md.is_some(), "should have CLAUDE.md");
        let content = String::from_utf8(claude_md.unwrap().content.clone()).unwrap();
        assert!(content.contains("Sandbox Environment"));
        assert!(content.contains("Python dev"));

        let tdd_skill = files.iter().find(|f| f.relative_path == "claude/skills/tdd/SKILL.md");
        assert!(tdd_skill.is_some(), "should have tdd SKILL.md");
        let tdd_content = String::from_utf8(tdd_skill.unwrap().content.clone()).unwrap();
        assert!(tdd_content.contains("name: tdd"));
        assert!(tdd_content.contains("Red-green-refactor"));
    }

    #[test]
    fn test_generate_goose_skills() {
        let skills = sample_skills();
        let files = SkillsGenerator::generate_all(&skills, &AgentType::Goose, "python-dev", "You are a Python dev.");

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "goose/.goosehints");

        let content = String::from_utf8(files[0].content.clone()).unwrap();
        assert!(content.contains("Sandbox Environment"));
        assert!(content.contains("## tdd"));
        assert!(content.contains("## git-workflow"));
        assert!(content.contains("Red-green-refactor"));
    }

    #[test]
    fn test_generate_codex_skills() {
        let skills = sample_skills();
        let files = SkillsGenerator::generate_all(&skills, &AgentType::Codex, "python-dev", "You are a Python dev.");

        // 2 skill files + 1 AGENTS.md
        assert_eq!(files.len(), 3);

        let agents_md = files.iter().find(|f| f.relative_path == "codex/AGENTS.md");
        assert!(agents_md.is_some());

        let tdd_skill = files.iter().find(|f| f.relative_path == "agents/skills/tdd/SKILL.md");
        assert!(tdd_skill.is_some());
    }

    #[test]
    fn test_generate_cursor_skills() {
        let skills = sample_skills();
        let files = SkillsGenerator::generate_all(&skills, &AgentType::Cursor, "python-dev", "You are a Python dev.");

        // 2 .mdc rule files + 1 preamble rule
        assert_eq!(files.len(), 3);

        let preamble = files.iter().find(|f| f.relative_path == "cursor/rules/nanosandbox-agent.mdc");
        assert!(preamble.is_some());
        let preamble_content = String::from_utf8(preamble.unwrap().content.clone()).unwrap();
        assert!(preamble_content.contains("alwaysApply: true"));

        let tdd_rule = files.iter().find(|f| f.relative_path == "cursor/rules/nanosb-tdd.mdc");
        assert!(tdd_rule.is_some());
        let tdd_content = String::from_utf8(tdd_rule.unwrap().content.clone()).unwrap();
        assert!(tdd_content.contains("alwaysApply: false"));
        assert!(tdd_content.contains("description: \"Test-driven development\""));
    }

    #[test]
    fn test_generate_empty_skills() {
        let files = SkillsGenerator::generate_all(&[], &AgentType::Claude, "test", "prompt");
        // Even with empty skills, we still get the prompt file
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "claude/CLAUDE.md");
    }

    #[test]
    fn test_format_skill_md() {
        let skill = SkillDef {
            name: "test".to_string(),
            description: "A test".to_string(),
            content: "Content here.".to_string(),
            version: "2.0".to_string(),
            tags: vec![],
            when_to_use: "Use when testing".to_string(),
            allowed_tools: vec!["bash".to_string()],
            user_invocable: Some(false),
            paths: vec!["*.rs".to_string()],
        };
        let formatted = SkillsGenerator::format_skill_md(&skill);
        assert!(formatted.contains("name: test"));
        assert!(formatted.contains("version: \"2.0\""));
        assert!(formatted.contains("when_to_use: \"Use when testing\""));
        assert!(formatted.contains("allowed-tools: bash"));
        assert!(formatted.contains("user-invocable: false"));
        assert!(formatted.contains("paths:"));
        assert!(formatted.contains("Content here."));
    }
}
