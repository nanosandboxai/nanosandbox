package main

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/nanosandboxai/agent-gateway/mcp"
	"github.com/nanosandboxai/agent-gateway/skills"
)

func TestSaveLoadGatewayState_RoundTrip(t *testing.T) {
	dir := t.TempDir()
	stateFile := filepath.Join(dir, "state.json")

	mcpMgr := mcp.NewManager()
	mcpMgr.AddServer("github", &mcp.McpServerDef{
		Command: "npx",
		Args:    []string{"-y", "@modelcontextprotocol/server-github"},
		Enabled: true,
	})

	skillsMgr := skills.NewManager()
	skillsMgr.AddSkill("tdd", &skills.SkillDef{
		Name:        "tdd",
		Description: "Test-driven development",
		Content:     "# TDD\n\nRed-green-refactor.",
	})
	skillsMgr.SetAgentDefinition("claude", "You are a helpful assistant.")

	if err := saveGatewayState(mcpMgr, skillsMgr, stateFile); err != nil {
		t.Fatalf("saveGatewayState: %v", err)
	}

	if _, err := os.Stat(stateFile); err != nil {
		t.Fatalf("state file not written: %v", err)
	}

	mcpMgr2 := mcp.NewManager()
	skillsMgr2 := skills.NewManager()
	if err := loadGatewayState(mcpMgr2, skillsMgr2, stateFile); err != nil {
		t.Fatalf("loadGatewayState: %v", err)
	}

	if mcpMgr2.GetServer("github") == nil {
		t.Error("expected github MCP server after load")
	}
	if skillsMgr2.GetSkill("tdd") == nil {
		t.Error("expected tdd skill after load")
	}
	name, _ := skillsMgr2.GetAgentDefinition()
	if name != "claude" {
		t.Errorf("expected agent name 'claude', got %q", name)
	}
}

func TestLoadGatewayState_MissingFile(t *testing.T) {
	mcpMgr := mcp.NewManager()
	skillsMgr := skills.NewManager()
	err := loadGatewayState(mcpMgr, skillsMgr, "/nonexistent/path/state.json")
	if err != nil {
		t.Errorf("expected no error for missing file, got %v", err)
	}
}

func TestSaveGatewayState_CreatesParentDir(t *testing.T) {
	dir := t.TempDir()
	stateFile := filepath.Join(dir, "subdir", "state.json")

	mcpMgr := mcp.NewManager()
	skillsMgr := skills.NewManager()
	if err := saveGatewayState(mcpMgr, skillsMgr, stateFile); err != nil {
		t.Fatalf("saveGatewayState should create parent dirs: %v", err)
	}
	if _, err := os.Stat(stateFile); err != nil {
		t.Error("state file not created")
	}
}
