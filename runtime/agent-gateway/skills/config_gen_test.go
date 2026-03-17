package skills

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

var testSkills = map[string]*SkillDef{
	"git-workflow": {
		Name:        "git-workflow",
		Description: "Git workflow best practices with conventional commits",
		Content:     "# Git Workflow\n\nUse conventional commits.",
	},
	"tdd": {
		Name:        "tdd",
		Description: "Test-driven development",
		Content:     "# TDD\n\nRed-green-refactor cycle.",
	},
}

func TestGenerateSkillMDFiles(t *testing.T) {
	dir := t.TempDir()

	if err := generateSkillMDFiles(dir, testSkills); err != nil {
		t.Fatalf("generateSkillMDFiles failed: %v", err)
	}

	// Check git-workflow
	path := filepath.Join(dir, "git-workflow", "SKILL.md")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("failed to read %s: %v", path, err)
	}
	content := string(data)

	if !strings.Contains(content, "---\nname: git-workflow\n") {
		t.Error("expected YAML frontmatter with name")
	}
	if !strings.Contains(content, "description: Git workflow best practices") {
		t.Error("expected description in frontmatter")
	}
	if !strings.Contains(content, "# Git Workflow") {
		t.Error("expected markdown body")
	}

	// Check tdd
	path = filepath.Join(dir, "tdd", "SKILL.md")
	data, err = os.ReadFile(path)
	if err != nil {
		t.Fatalf("failed to read %s: %v", path, err)
	}
	if !strings.Contains(string(data), "Red-green-refactor") {
		t.Error("expected tdd content")
	}
}

func TestGenerateSkillMDFiles_Empty(t *testing.T) {
	dir := t.TempDir()
	if err := generateSkillMDFiles(dir, map[string]*SkillDef{}); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestGenerateSkillMDFiles_EmptyDir(t *testing.T) {
	if err := generateSkillMDFiles("", testSkills); err != nil {
		t.Fatalf("expected nil for empty dir: %v", err)
	}
}

func TestGenerateClaudePrompt(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "CLAUDE.md")

	if err := generateClaudePrompt(path, "python-developer", "You are a Python expert."); err != nil {
		t.Fatalf("failed: %v", err)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("failed to read: %v", err)
	}
	content := string(data)

	if !strings.Contains(content, "<!-- nanosandbox agent: python-developer -->") {
		t.Error("expected agent marker comment")
	}
	if !strings.Contains(content, "You are a Python expert.") {
		t.Error("expected prompt content")
	}
}

func TestGenerateClaudePrompt_Empty(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "CLAUDE.md")

	if err := generateClaudePrompt(path, "", ""); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	// File should still be created with sandbox preamble even when both are empty.
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("failed to read: %v", err)
	}
	if !strings.Contains(string(data), "Sandbox Environment") {
		t.Error("expected sandbox preamble in empty prompt file")
	}
}

func TestGenerateGooseAll(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, ".goosehints")

	err := generateGooseAll(path, "python-developer", "You are a Python expert.", testSkills)
	if err != nil {
		t.Fatalf("failed: %v", err)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("failed to read: %v", err)
	}
	content := string(data)

	if !strings.Contains(content, "<!-- nanosandbox agent: python-developer -->") {
		t.Error("expected agent marker")
	}
	if !strings.Contains(content, "You are a Python expert.") {
		t.Error("expected prompt content")
	}
	if !strings.Contains(content, "<!-- nanosandbox skill: git-workflow -->") {
		t.Error("expected git-workflow skill marker")
	}
	if !strings.Contains(content, "<!-- nanosandbox skill: tdd -->") {
		t.Error("expected tdd skill marker")
	}
	if !strings.Contains(content, "Red-green-refactor") {
		t.Error("expected tdd content")
	}

	// Skills should be alphabetically sorted
	gitIdx := strings.Index(content, "nanosandbox skill: git-workflow")
	tddIdx := strings.Index(content, "nanosandbox skill: tdd")
	if gitIdx > tddIdx {
		t.Error("expected skills in alphabetical order (git-workflow before tdd)")
	}
}

func TestGenerateGooseAll_Empty(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, ".goosehints")

	if err := generateGooseAll(path, "", "", map[string]*SkillDef{}); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	// File should still be created with sandbox preamble even when everything else is empty.
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("failed to read: %v", err)
	}
	if !strings.Contains(string(data), "Sandbox Environment") {
		t.Error("expected sandbox preamble in empty prompt file")
	}
}

func TestGenerateCodexPrompt(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "AGENTS.md")

	if err := generateCodexPrompt(path, "rust-developer", "You write idiomatic Rust."); err != nil {
		t.Fatalf("failed: %v", err)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("failed to read: %v", err)
	}
	content := string(data)

	if !strings.Contains(content, "<!-- nanosandbox agent: rust-developer -->") {
		t.Error("expected agent marker")
	}
	if !strings.Contains(content, "You write idiomatic Rust.") {
		t.Error("expected prompt content")
	}
}

func TestGenerateCursorPrompt(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "rules", "nanosandbox-agent.mdc")

	if err := generateCursorPrompt(path, "react-developer", "You build React apps."); err != nil {
		t.Fatalf("failed: %v", err)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("failed to read: %v", err)
	}
	content := string(data)

	if !strings.Contains(content, "---\n") {
		t.Error("expected YAML frontmatter delimiters")
	}
	if !strings.Contains(content, "alwaysApply: true") {
		t.Error("expected alwaysApply: true in frontmatter")
	}
	if !strings.Contains(content, `description: "Nanosandbox agent definition: react-developer"`) {
		t.Error("expected description with agent name")
	}
	if !strings.Contains(content, "You build React apps.") {
		t.Error("expected prompt content after frontmatter")
	}
}

func TestGenerateCursorPrompt_Empty(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "rules", "nanosandbox-agent.mdc")

	if err := generateCursorPrompt(path, "", ""); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	// File should still be created with sandbox preamble + frontmatter even when both are empty.
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("failed to read: %v", err)
	}
	if !strings.Contains(string(data), "Sandbox Environment") {
		t.Error("expected sandbox preamble in empty prompt file")
	}
}

func TestGenerateAllConfigs(t *testing.T) {
	// Override agentConfigs with temp dirs for this test
	origConfigs := make(map[string]*AgentSkillConfig)
	for k, v := range agentConfigs {
		cp := *v
		origConfigs[k] = &cp
	}

	dir := t.TempDir()
	agentConfigs["claude"] = &AgentSkillConfig{
		Format:     "claude",
		SkillsDir:  filepath.Join(dir, "claude", "skills"),
		PromptFile: filepath.Join(dir, "claude", "CLAUDE.md"),
	}
	agentConfigs["goose"] = &AgentSkillConfig{
		Format:     "goose",
		SkillsDir:  "",
		PromptFile: filepath.Join(dir, "goose", ".goosehints"),
	}
	agentConfigs["codex"] = &AgentSkillConfig{
		Format:     "codex",
		SkillsDir:  filepath.Join(dir, "codex", "skills"),
		PromptFile: filepath.Join(dir, "codex", "AGENTS.md"),
	}
	agentConfigs["cursor"] = &AgentSkillConfig{
		Format:     "cursor",
		SkillsDir:  filepath.Join(dir, "cursor", "skills"),
		PromptFile: filepath.Join(dir, "cursor", "rules", "nanosandbox-agent.mdc"),
	}

	// Restore after test
	defer func() {
		for k, v := range origConfigs {
			agentConfigs[k] = v
		}
	}()

	mgr := NewManager()
	mgr.SetAgentDefinition("python-developer", "You are a Python developer.")
	mgr.AddSkill("tdd", &SkillDef{
		Name:        "tdd",
		Description: "Test-driven development",
		Content:     "# TDD\n\nRed-green-refactor.",
	})

	if err := mgr.GenerateAllConfigs(); err != nil {
		t.Fatalf("GenerateAllConfigs failed: %v", err)
	}

	// Verify Claude: SKILL.md + CLAUDE.md
	claudeSkill := filepath.Join(dir, "claude", "skills", "tdd", "SKILL.md")
	if _, err := os.Stat(claudeSkill); os.IsNotExist(err) {
		t.Error("expected Claude SKILL.md to exist")
	}
	claudePrompt := filepath.Join(dir, "claude", "CLAUDE.md")
	if _, err := os.Stat(claudePrompt); os.IsNotExist(err) {
		t.Error("expected Claude CLAUDE.md to exist")
	}

	// Verify Goose: .goosehints with both prompt and skill
	gooseHints := filepath.Join(dir, "goose", ".goosehints")
	data, err := os.ReadFile(gooseHints)
	if err != nil {
		t.Fatalf("failed to read .goosehints: %v", err)
	}
	gooseContent := string(data)
	if !strings.Contains(gooseContent, "nanosandbox agent: python-developer") {
		t.Error("expected agent marker in .goosehints")
	}
	if !strings.Contains(gooseContent, "nanosandbox skill: tdd") {
		t.Error("expected skill marker in .goosehints")
	}

	// Verify Codex: SKILL.md + AGENTS.md
	codexSkill := filepath.Join(dir, "codex", "skills", "tdd", "SKILL.md")
	if _, err := os.Stat(codexSkill); os.IsNotExist(err) {
		t.Error("expected Codex SKILL.md to exist")
	}
	codexPrompt := filepath.Join(dir, "codex", "AGENTS.md")
	if _, err := os.Stat(codexPrompt); os.IsNotExist(err) {
		t.Error("expected Codex AGENTS.md to exist")
	}

	// Verify Cursor: SKILL.md + .mdc rule
	cursorSkill := filepath.Join(dir, "cursor", "skills", "tdd", "SKILL.md")
	if _, err := os.Stat(cursorSkill); os.IsNotExist(err) {
		t.Error("expected Cursor SKILL.md to exist")
	}
	cursorPrompt := filepath.Join(dir, "cursor", "rules", "nanosandbox-agent.mdc")
	data, err = os.ReadFile(cursorPrompt)
	if err != nil {
		t.Fatalf("failed to read cursor prompt: %v", err)
	}
	if !strings.Contains(string(data), "alwaysApply: true") {
		t.Error("expected alwaysApply in cursor mdc file")
	}
}

func TestGenerateAllConfigs_FilteredByType(t *testing.T) {
	// Override agentConfigs with temp dirs for this test
	origConfigs := make(map[string]*AgentSkillConfig)
	for k, v := range agentConfigs {
		cp := *v
		origConfigs[k] = &cp
	}

	dir := t.TempDir()
	agentConfigs["claude"] = &AgentSkillConfig{
		Format:     "claude",
		SkillsDir:  filepath.Join(dir, "claude", "skills"),
		PromptFile: filepath.Join(dir, "claude", "CLAUDE.md"),
	}
	agentConfigs["goose"] = &AgentSkillConfig{
		Format:     "goose",
		SkillsDir:  "",
		PromptFile: filepath.Join(dir, "goose", ".goosehints"),
	}
	agentConfigs["codex"] = &AgentSkillConfig{
		Format:     "codex",
		SkillsDir:  filepath.Join(dir, "codex", "skills"),
		PromptFile: filepath.Join(dir, "codex", "AGENTS.md"),
	}
	agentConfigs["cursor"] = &AgentSkillConfig{
		Format:     "cursor",
		SkillsDir:  filepath.Join(dir, "cursor", "skills"),
		PromptFile: filepath.Join(dir, "cursor", "rules", "nanosandbox-agent.mdc"),
	}

	defer func() {
		for k, v := range origConfigs {
			agentConfigs[k] = v
		}
	}()

	mgr := NewManager()
	mgr.SetAgentType("claude")
	mgr.SetAgentDefinition("python-developer", "You are a Python developer.")
	mgr.AddSkill("tdd", &SkillDef{
		Name:        "tdd",
		Description: "Test-driven development",
		Content:     "# TDD\n\nRed-green-refactor.",
	})

	if err := mgr.GenerateAllConfigs(); err != nil {
		t.Fatalf("GenerateAllConfigs failed: %v", err)
	}

	// Claude files SHOULD exist
	claudePrompt := filepath.Join(dir, "claude", "CLAUDE.md")
	if _, err := os.Stat(claudePrompt); os.IsNotExist(err) {
		t.Error("expected Claude CLAUDE.md to exist")
	}
	claudeSkill := filepath.Join(dir, "claude", "skills", "tdd", "SKILL.md")
	if _, err := os.Stat(claudeSkill); os.IsNotExist(err) {
		t.Error("expected Claude SKILL.md to exist")
	}

	// Other agent files should NOT exist
	gooseHints := filepath.Join(dir, "goose", ".goosehints")
	if _, err := os.Stat(gooseHints); !os.IsNotExist(err) {
		t.Error("expected goose .goosehints to NOT exist when agent type is claude")
	}
	codexPrompt := filepath.Join(dir, "codex", "AGENTS.md")
	if _, err := os.Stat(codexPrompt); !os.IsNotExist(err) {
		t.Error("expected codex AGENTS.md to NOT exist when agent type is claude")
	}
	cursorPrompt := filepath.Join(dir, "cursor", "rules", "nanosandbox-agent.mdc")
	if _, err := os.Stat(cursorPrompt); !os.IsNotExist(err) {
		t.Error("expected cursor .mdc to NOT exist when agent type is claude")
	}
}
