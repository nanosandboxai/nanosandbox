package mcp

import (
	"os"
	"testing"
)

func TestNewManagerFromBytes(t *testing.T) {
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
  fetch:
    command: "npx"
    args: ["-y", "mcp-server-fetch"]
    enabled: true
agents:
  claude:
    config_path: "/tmp/test-claude.json"
    format: "claude"
    exclude: []
`)

	mgr, err := NewManagerFromBytes(data)
	if err != nil {
		t.Fatalf("NewManagerFromBytes failed: %v", err)
	}

	servers := mgr.ListServers()
	if len(servers) != 2 {
		t.Fatalf("expected 2 servers, got %d", len(servers))
	}
}

func TestEnvVarResolution(t *testing.T) {
	os.Setenv("TEST_MCP_TOKEN", "my-secret-token")
	defer os.Unsetenv("TEST_MCP_TOKEN")

	data := []byte(`
version: "1"
servers:
  test-server:
    command: "echo"
    args: ["hello"]
    env:
      API_KEY: "${TEST_MCP_TOKEN}"
    enabled: true
agents: {}
`)

	mgr, err := NewManagerFromBytes(data)
	if err != nil {
		t.Fatalf("NewManagerFromBytes failed: %v", err)
	}

	srv := mgr.GetServer("test-server")
	if srv == nil {
		t.Fatal("expected test-server")
	}
	if srv.Env["API_KEY"] != "my-secret-token" {
		t.Errorf("expected resolved token, got %q", srv.Env["API_KEY"])
	}
}

func TestUnresolvableEnvVarKept(t *testing.T) {
	os.Unsetenv("NONEXISTENT_VAR_XYZ")

	data := []byte(`
version: "1"
servers:
  test-server:
    command: "echo"
    args: []
    env:
      KEY: "${NONEXISTENT_VAR_XYZ}"
    enabled: true
agents: {}
`)

	mgr, err := NewManagerFromBytes(data)
	if err != nil {
		t.Fatalf("NewManagerFromBytes failed: %v", err)
	}

	srv := mgr.GetServer("test-server")
	if srv.Env["KEY"] != "" {
		t.Errorf("expected empty string for unresolvable var, got %q", srv.Env["KEY"])
	}
}

func TestAddRemoveServer(t *testing.T) {
	data := []byte(`
version: "1"
servers:
  existing:
    command: "echo"
    args: ["hi"]
    enabled: true
agents: {}
`)

	mgr, err := NewManagerFromBytes(data)
	if err != nil {
		t.Fatalf("NewManagerFromBytes failed: %v", err)
	}

	mgr.AddServer("new-server", &McpServerDef{
		Command: "npx",
		Args:    []string{"-y", "some-package"},
		Enabled: true,
	})

	if mgr.GetServer("new-server") == nil {
		t.Error("expected new-server after add")
	}
	if len(mgr.ListServers()) != 2 {
		t.Errorf("expected 2 servers, got %d", len(mgr.ListServers()))
	}

	mgr.RemoveServer("new-server")
	if mgr.GetServer("new-server") != nil {
		t.Error("expected new-server to be removed")
	}
}

func TestEnableDisableServer(t *testing.T) {
	data := []byte(`
version: "1"
servers:
  srv:
    command: "echo"
    args: []
    enabled: true
agents: {}
`)

	mgr, err := NewManagerFromBytes(data)
	if err != nil {
		t.Fatalf("NewManagerFromBytes failed: %v", err)
	}

	mgr.DisableServer("srv")
	if mgr.GetServer("srv").Enabled {
		t.Error("expected srv to be disabled")
	}

	mgr.EnableServer("srv")
	if !mgr.GetServer("srv").Enabled {
		t.Error("expected srv to be enabled")
	}
}

func TestClaudeMcpPath(t *testing.T) {
	mgr := NewManager()
	cfg := mgr.AgentConfig("claude")
	if cfg == nil {
		t.Fatal("expected claude agent config")
	}
	const want = "/home/developer/.claude.json"
	if cfg.ConfigPath != want {
		t.Errorf("Claude MCP ConfigPath: got %q, want %q", cfg.ConfigPath, want)
	}
}

func TestEnabledServersForAgent(t *testing.T) {
	data := []byte(`
version: "1"
servers:
  github:
    command: "npx"
    args: ["-y", "github"]
    enabled: true
  filesystem:
    command: "npx"
    args: ["-y", "fs"]
    enabled: true
  disabled-one:
    command: "echo"
    args: []
    enabled: false
agents:
  claude:
    config_path: "/tmp/test.json"
    format: "claude"
    exclude: [filesystem]
`)

	mgr, err := NewManagerFromBytes(data)
	if err != nil {
		t.Fatalf("NewManagerFromBytes failed: %v", err)
	}

	servers := mgr.EnabledServersForAgent("claude")
	if len(servers) != 1 {
		names := make([]string, 0)
		for k := range servers {
			names = append(names, k)
		}
		t.Fatalf("expected 1 server for claude, got %d: %v", len(servers), names)
	}
	if _, ok := servers["github"]; !ok {
		t.Error("expected github in claude's servers")
	}
}
