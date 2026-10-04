package skills

import (
	"strings"
	"testing"
)

func TestNewManager(t *testing.T) {
	mgr := NewManager()
	if mgr == nil {
		t.Fatal("expected non-nil manager")
	}
	if len(mgr.ListSkills()) != 0 {
		t.Error("expected empty skills map")
	}
}

func TestAddRemoveSkill(t *testing.T) {
	mgr := NewManager()

	mgr.AddSkill("tdd", &SkillDef{
		Name:        "tdd",
		Description: "Test-driven development",
		Content:     "# TDD\n\nRed-green-refactor cycle.",
	})

	if mgr.GetSkill("tdd") == nil {
		t.Error("expected tdd after add")
	}
	if len(mgr.ListSkills()) != 1 {
		t.Errorf("expected 1 skill, got %d", len(mgr.ListSkills()))
	}

	mgr.RemoveSkill("tdd")
	if mgr.GetSkill("tdd") != nil {
		t.Error("expected tdd to be removed")
	}
	if len(mgr.ListSkills()) != 0 {
		t.Errorf("expected 0 skills, got %d", len(mgr.ListSkills()))
	}
}

func TestGetSkillReturnsCopy(t *testing.T) {
	mgr := NewManager()
	mgr.AddSkill("git", &SkillDef{
		Name:    "git",
		Content: "original",
	})

	s := mgr.GetSkill("git")
	s.Content = "modified"

	original := mgr.GetSkill("git")
	if original.Content != "original" {
		t.Error("GetSkill should return a copy, original was mutated")
	}
}

func TestGetSkillNotFound(t *testing.T) {
	mgr := NewManager()
	if mgr.GetSkill("nonexistent") != nil {
		t.Error("expected nil for nonexistent skill")
	}
}

func TestListSkillsReturnsCopies(t *testing.T) {
	mgr := NewManager()
	mgr.AddSkill("a", &SkillDef{Name: "a", Content: "content-a"})
	mgr.AddSkill("b", &SkillDef{Name: "b", Content: "content-b"})

	skills := mgr.ListSkills()
	if len(skills) != 2 {
		t.Fatalf("expected 2 skills, got %d", len(skills))
	}
	if skills["a"] == nil || skills["b"] == nil {
		t.Error("expected both skills in list")
	}
}

func TestSetAgentDefinition(t *testing.T) {
	mgr := NewManager()

	mgr.SetAgentDefinition("python-developer", "You are a Python developer.")

	name, prompt := mgr.GetAgentDefinition()
	if name != "python-developer" {
		t.Errorf("expected name python-developer, got %q", name)
	}
	if prompt != "You are a Python developer." {
		t.Errorf("expected prompt, got %q", prompt)
	}
}

func TestSetAgentDefinitionOverwrite(t *testing.T) {
	mgr := NewManager()

	mgr.SetAgentDefinition("python-developer", "Python prompt")
	mgr.SetAgentDefinition("rust-developer", "Rust prompt")

	name, prompt := mgr.GetAgentDefinition()
	if name != "rust-developer" {
		t.Errorf("expected rust-developer, got %q", name)
	}
	if prompt != "Rust prompt" {
		t.Errorf("expected Rust prompt, got %q", prompt)
	}
}

func TestGetAgentDefinitionEmpty(t *testing.T) {
	mgr := NewManager()
	name, prompt := mgr.GetAgentDefinition()
	if name != "" || prompt != "" {
		t.Error("expected empty agent definition")
	}
}

func TestAgentConfigPaths_HomeNotWorkspace(t *testing.T) {
	for agentName, cfg := range agentConfigs {
		if strings.Contains(cfg.PromptFile, "/workspace") {
			t.Errorf("agent %q: PromptFile must not be in /workspace, got %q — would pollute git", agentName, cfg.PromptFile)
		}
		if cfg.SkillsDir != "" && strings.Contains(cfg.SkillsDir, "/workspace") {
			t.Errorf("agent %q: SkillsDir must not be in /workspace, got %q — would pollute git", agentName, cfg.SkillsDir)
		}
	}
}
