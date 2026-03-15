package skills

import (
	"fmt"
	"log"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

// GenerateAllConfigs writes skill files and prompt files for all configured agents.
func (m *Manager) GenerateAllConfigs() error {
	m.mu.RLock()
	defer m.mu.RUnlock()

	for agentName, agentCfg := range agentConfigs {
		if err := m.generateForAgentLocked(agentName, agentCfg); err != nil {
			return fmt.Errorf("generating skills for %s: %w", agentName, err)
		}
	}
	return nil
}

func (m *Manager) generateForAgentLocked(agentName string, cfg *AgentSkillConfig) error {
	switch cfg.Format {
	case "claude":
		if err := generateClaudeSkills(cfg.SkillsDir, m.skills); err != nil {
			return err
		}
		return generateClaudePrompt(cfg.PromptFile, m.agentName, m.agentPrompt)
	case "goose":
		return generateGooseAll(cfg.PromptFile, m.agentName, m.agentPrompt, m.skills)
	case "codex":
		if err := generateSkillMDFiles(cfg.SkillsDir, m.skills); err != nil {
			return err
		}
		return generateCodexPrompt(cfg.PromptFile, m.agentName, m.agentPrompt)
	case "cursor":
		if err := generateSkillMDFiles(cfg.SkillsDir, m.skills); err != nil {
			return err
		}
		return generateCursorPrompt(cfg.PromptFile, m.agentName, m.agentPrompt)
	default:
		log.Printf("[skills] unknown format %q for agent %s, skipping", cfg.Format, agentName)
		return nil
	}
}

// --- Claude Code Format ---
// Skills: /workspace/.claude/skills/<name>/SKILL.md
// Prompt: /workspace/CLAUDE.md

func generateClaudeSkills(skillsDir string, skills map[string]*SkillDef) error {
	return generateSkillMDFiles(skillsDir, skills)
}

func generateClaudePrompt(path, agentName, prompt string) error {
	if agentName == "" && prompt == "" {
		return nil
	}
	content := fmt.Sprintf("<!-- nanosandbox agent: %s -->\n\n%s\n", agentName, prompt)
	return writeFile(path, []byte(content))
}

// --- Goose Format ---
// Goose has no native SKILL.md. Everything goes into .goosehints.
// Prompt + skills concatenated into a single file.

func generateGooseAll(path, agentName, prompt string, skills map[string]*SkillDef) error {
	var b strings.Builder

	// Agent prompt section
	if agentName != "" || prompt != "" {
		b.WriteString(fmt.Sprintf("<!-- nanosandbox agent: %s -->\n", agentName))
		b.WriteString(prompt)
		b.WriteString("\n\n")
	}

	// Skills sections
	names := sortedSkillNames(skills)
	for _, name := range names {
		skill := skills[name]
		b.WriteString(fmt.Sprintf("<!-- nanosandbox skill: %s -->\n", name))
		b.WriteString(fmt.Sprintf("## %s\n\n", skill.Name))
		if skill.Description != "" {
			b.WriteString(skill.Description + "\n\n")
		}
		b.WriteString(skill.Content)
		b.WriteString("\n\n")
	}

	if b.Len() == 0 {
		return nil
	}
	return writeFile(path, []byte(b.String()))
}

// --- Codex Format ---
// Skills: /workspace/.agents/skills/<name>/SKILL.md (same as Claude)
// Prompt: /workspace/AGENTS.md

func generateCodexPrompt(path, agentName, prompt string) error {
	if agentName == "" && prompt == "" {
		return nil
	}
	content := fmt.Sprintf("<!-- nanosandbox agent: %s -->\n\n%s\n", agentName, prompt)
	return writeFile(path, []byte(content))
}

// --- Cursor Format ---
// Skills: /workspace/.cursor/skills/<name>/SKILL.md (same as Claude)
// Prompt: /workspace/.cursor/rules/nanosandbox-agent.mdc (with alwaysApply frontmatter)

func generateCursorPrompt(path, agentName, prompt string) error {
	if agentName == "" && prompt == "" {
		return nil
	}
	var b strings.Builder
	b.WriteString("---\n")
	b.WriteString(fmt.Sprintf("description: \"Nanosandbox agent definition: %s\"\n", agentName))
	b.WriteString("alwaysApply: true\n")
	b.WriteString("---\n\n")
	b.WriteString(prompt)
	b.WriteString("\n")
	return writeFile(path, []byte(b.String()))
}

// --- Shared: SKILL.md file generation for Claude/Codex/Cursor ---

func generateSkillMDFiles(skillsDir string, skills map[string]*SkillDef) error {
	if skillsDir == "" {
		return nil
	}

	// Clean existing nanosandbox-managed skill dirs
	// (only remove dirs we manage, not user-created ones)
	for name := range skills {
		dir := filepath.Join(skillsDir, name)
		_ = os.MkdirAll(dir, 0755)
	}

	for name, skill := range skills {
		dir := filepath.Join(skillsDir, name)
		if err := os.MkdirAll(dir, 0755); err != nil {
			return fmt.Errorf("creating skill dir %s: %w", dir, err)
		}

		var b strings.Builder
		b.WriteString("---\n")
		b.WriteString(fmt.Sprintf("name: %s\n", skill.Name))
		b.WriteString(fmt.Sprintf("description: %s\n", skill.Description))
		b.WriteString("---\n\n")
		b.WriteString(skill.Content)
		b.WriteString("\n")

		path := filepath.Join(dir, "SKILL.md")
		if err := os.WriteFile(path, []byte(b.String()), 0644); err != nil {
			return fmt.Errorf("writing %s: %w", path, err)
		}
		log.Printf("[skills] wrote %s", path)
	}

	return nil
}

// --- Helpers ---

func writeFile(path string, data []byte) error {
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0755); err != nil {
		return fmt.Errorf("creating dir %s: %w", dir, err)
	}
	if err := os.WriteFile(path, data, 0644); err != nil {
		return fmt.Errorf("writing %s: %w", path, err)
	}
	log.Printf("[skills] wrote %s", path)
	return nil
}

func sortedSkillNames(skills map[string]*SkillDef) []string {
	names := make([]string, 0, len(skills))
	for k := range skills {
		names = append(names, k)
	}
	sort.Strings(names)
	return names
}
