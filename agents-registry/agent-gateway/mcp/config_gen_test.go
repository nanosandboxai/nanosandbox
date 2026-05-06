package mcp

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

var testServers = map[string]*McpServerDef{
	"github": {
		Command: "npx",
		Args:    []string{"-y", "@modelcontextprotocol/server-github"},
		Env:     map[string]string{"GITHUB_PERSONAL_ACCESS_TOKEN": "test-token"},
		Enabled: true,
	},
	"fetch": {
		Command: "npx",
		Args:    []string{"-y", "mcp-server-fetch"},
		Enabled: true,
	},
}

func TestGenerateClaudeConfig(t *testing.T) {
	out, err := GenerateClaudeConfig(testServers)
	if err != nil {
		t.Fatalf("GenerateClaudeConfig failed: %v", err)
	}

	var parsed map[string]interface{}
	if err := json.Unmarshal(out, &parsed); err != nil {
		t.Fatalf("invalid JSON: %v\n%s", err, out)
	}

	mcpServers, ok := parsed["mcpServers"].(map[string]interface{})
	if !ok {
		t.Fatalf("expected mcpServers object, got %T", parsed["mcpServers"])
	}

	if _, ok := mcpServers["github"]; !ok {
		t.Error("expected github in mcpServers")
	}
	if _, ok := mcpServers["fetch"]; !ok {
		t.Error("expected fetch in mcpServers")
	}

	gh := mcpServers["github"].(map[string]interface{})
	env := gh["env"].(map[string]interface{})
	if env["GITHUB_PERSONAL_ACCESS_TOKEN"] != "test-token" {
		t.Errorf("unexpected token: %v", env["GITHUB_PERSONAL_ACCESS_TOKEN"])
	}
}

func TestGenerateGooseConfig(t *testing.T) {
	out, err := GenerateGooseConfig(testServers)
	if err != nil {
		t.Fatalf("GenerateGooseConfig failed: %v", err)
	}

	yamlStr := string(out)

	if !strings.Contains(yamlStr, "extensions:") {
		t.Error("expected 'extensions:' in goose config")
	}
	if !strings.Contains(yamlStr, "github:") {
		t.Error("expected 'github:' in goose config")
	}
	if !strings.Contains(yamlStr, "type: stdio") {
		t.Error("expected 'type: stdio' in goose config")
	}
	if !strings.Contains(yamlStr, "cmd: npx") {
		t.Error("expected 'cmd: npx' in goose config")
	}
}

func TestGenerateCodexConfig(t *testing.T) {
	out, err := GenerateCodexConfig(testServers)
	if err != nil {
		t.Fatalf("GenerateCodexConfig failed: %v", err)
	}

	tomlStr := string(out)

	if !strings.Contains(tomlStr, "[mcp_servers.github]") {
		t.Error("expected [mcp_servers.github] section")
	}
	if !strings.Contains(tomlStr, `command = "npx"`) {
		t.Error("expected command = \"npx\"")
	}
	if !strings.Contains(tomlStr, "[mcp_servers.fetch]") {
		t.Error("expected [mcp_servers.fetch] section")
	}
}

func TestGenerateCursorConfig(t *testing.T) {
	out, err := GenerateCursorConfig(testServers)
	if err != nil {
		t.Fatalf("GenerateCursorConfig failed: %v", err)
	}

	var parsed map[string]interface{}
	if err := json.Unmarshal(out, &parsed); err != nil {
		t.Fatalf("invalid JSON: %v\n%s", err, out)
	}

	mcpServers, ok := parsed["mcpServers"].(map[string]interface{})
	if !ok {
		t.Fatalf("expected mcpServers, got %T", parsed["mcpServers"])
	}
	if _, ok := mcpServers["github"]; !ok {
		t.Error("expected github in cursor config")
	}
}

func TestMergeClaudeSettings_PreservesExistingKeys(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "settings.json")

	// Write a pre-existing settings file with non-MCP keys.
	existing := `{"theme":"dark","hooks":{"PreToolUse":[]},"mcpServers":{}}`
	if err := os.WriteFile(path, []byte(existing), 0644); err != nil {
		t.Fatal(err)
	}

	if err := mergeClaudeSettings(path, testServers); err != nil {
		t.Fatalf("mergeClaudeSettings: %v", err)
	}

	data, _ := os.ReadFile(path)
	var parsed map[string]interface{}
	if err := json.Unmarshal(data, &parsed); err != nil {
		t.Fatalf("invalid JSON after merge: %v", err)
	}

	// Pre-existing keys must be preserved.
	if parsed["theme"] != "dark" {
		t.Errorf("theme key lost after merge, got %v", parsed["theme"])
	}
	if _, ok := parsed["hooks"]; !ok {
		t.Error("hooks key lost after merge")
	}

	// MCP servers must be updated.
	mcpServers, ok := parsed["mcpServers"].(map[string]interface{})
	if !ok {
		t.Fatalf("mcpServers not a map: %T", parsed["mcpServers"])
	}
	if _, ok := mcpServers["github"]; !ok {
		t.Error("expected github in merged mcpServers")
	}
}

func TestMergeClaudeSettings_CreatesFileIfAbsent(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "settings.json")

	if err := mergeClaudeSettings(path, testServers); err != nil {
		t.Fatalf("mergeClaudeSettings on new file: %v", err)
	}

	data, _ := os.ReadFile(path)
	var parsed map[string]interface{}
	if err := json.Unmarshal(data, &parsed); err != nil {
		t.Fatalf("invalid JSON: %v", err)
	}
	if _, ok := parsed["mcpServers"]; !ok {
		t.Error("expected mcpServers key in new file")
	}
}

func TestGenerateEmptyServers(t *testing.T) {
	empty := map[string]*McpServerDef{}

	if _, err := GenerateClaudeConfig(empty); err != nil {
		t.Errorf("claude: %v", err)
	}
	if _, err := GenerateGooseConfig(empty); err != nil {
		t.Errorf("goose: %v", err)
	}
	if _, err := GenerateCodexConfig(empty); err != nil {
		t.Errorf("codex: %v", err)
	}
	if _, err := GenerateCursorConfig(empty); err != nil {
		t.Errorf("cursor: %v", err)
	}
}
