package mcp

import (
	"testing"

	"gopkg.in/yaml.v3"
)

func TestParseMinimalConfig(t *testing.T) {
	data := []byte(`
version: "1"
defaults:
  timeout_sec: 30
  startup_timeout_sec: 10
servers:
  github:
    command: "npx"
    args: ["-y", "@modelcontextprotocol/server-github"]
    env:
      GITHUB_PERSONAL_ACCESS_TOKEN: "${GITHUB_TOKEN}"
    enabled: true
agents:
  claude:
    config_path: "/root/.claude.json"
    format: "claude"
    exclude: [filesystem]
`)

	var cfg McpConfig
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		t.Fatalf("failed to parse YAML: %v", err)
	}

	if cfg.Version != "1" {
		t.Errorf("expected version '1', got %q", cfg.Version)
	}
	if cfg.Defaults.TimeoutSec != 30 {
		t.Errorf("expected timeout 30, got %d", cfg.Defaults.TimeoutSec)
	}

	gh, ok := cfg.Servers["github"]
	if !ok {
		t.Fatal("expected 'github' server")
	}
	if gh.Command != "npx" {
		t.Errorf("expected command 'npx', got %q", gh.Command)
	}
	if !gh.Enabled {
		t.Error("expected github to be enabled")
	}
	if gh.Env["GITHUB_PERSONAL_ACCESS_TOKEN"] != "${GITHUB_TOKEN}" {
		t.Errorf("unexpected env value: %q", gh.Env["GITHUB_PERSONAL_ACCESS_TOKEN"])
	}

	claude, ok := cfg.Agents["claude"]
	if !ok {
		t.Fatal("expected 'claude' agent")
	}
	if claude.Format != "claude" {
		t.Errorf("expected format 'claude', got %q", claude.Format)
	}
	if len(claude.Exclude) != 1 || claude.Exclude[0] != "filesystem" {
		t.Errorf("unexpected exclude: %v", claude.Exclude)
	}
}

func TestServerDefDefaults(t *testing.T) {
	data := []byte(`
version: "1"
servers:
  fetch:
    command: "npx"
    args: ["-y", "mcp-server-fetch"]
`)
	var cfg McpConfig
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		t.Fatalf("parse error: %v", err)
	}
	fetch := cfg.Servers["fetch"]
	if fetch.Enabled {
		t.Error("expected enabled to default to false")
	}
}
