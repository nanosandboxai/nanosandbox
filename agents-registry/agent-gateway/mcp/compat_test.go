package mcp

import (
	"testing"
)

func TestAutoPopulateOverrides_UvxWithNpxAlt(t *testing.T) {
	// mcp-server-github has an npm equivalent.
	def := &McpServerDef{
		Command: "uvx",
		Args:    []string{"mcp-server-github"},
		Enabled: true,
	}
	AutoPopulateOverrides(def)

	for _, agent := range []string{"codex", "cursor"} {
		ov, ok := def.Overrides[agent]
		if !ok {
			t.Errorf("expected override for %s", agent)
			continue
		}
		if ov.Excluded {
			t.Errorf("%s: should not be excluded, has npm equivalent", agent)
		}
		if ov.Command != "npx" {
			t.Errorf("%s: expected command 'npx', got %q", agent, ov.Command)
		}
		if len(ov.Args) < 2 || ov.Args[0] != "-y" || ov.Args[1] != "@modelcontextprotocol/server-github" {
			t.Errorf("%s: expected args [-y @modelcontextprotocol/server-github], got %v", agent, ov.Args)
		}
	}
}

func TestAutoPopulateOverrides_UvxOnlyExcluded(t *testing.T) {
	// mcp-server-fetch is Python-only — no npm package.
	def := &McpServerDef{
		Command: "uvx",
		Args:    []string{"mcp-server-fetch"},
		Enabled: true,
	}
	AutoPopulateOverrides(def)

	for _, agent := range []string{"codex", "cursor"} {
		ov, ok := def.Overrides[agent]
		if !ok {
			t.Errorf("expected override for %s", agent)
			continue
		}
		if !ov.Excluded {
			t.Errorf("%s: should be excluded (uvx-only package)", agent)
		}
	}
}

func TestAutoPopulateOverrides_NpxNoOverride(t *testing.T) {
	def := &McpServerDef{
		Command: "npx",
		Args:    []string{"-y", "@modelcontextprotocol/server-github"},
		Enabled: true,
	}
	AutoPopulateOverrides(def)

	if len(def.Overrides) != 0 {
		t.Errorf("expected no overrides for npx command, got %v", def.Overrides)
	}
}

func TestAutoPopulateOverrides_UnknownUvxPassthrough(t *testing.T) {
	def := &McpServerDef{
		Command: "uvx",
		Args:    []string{"my-custom-mcp-server"},
		Enabled: true,
	}
	AutoPopulateOverrides(def)

	// Unknown package — no overrides, best-effort passthrough.
	if len(def.Overrides) != 0 {
		t.Errorf("expected no overrides for unknown uvx package, got %v", def.Overrides)
	}
}

func TestAutoPopulateOverrides_NonUvxNonNpx(t *testing.T) {
	def := &McpServerDef{
		Command: "/usr/local/bin/my-mcp",
		Args:    []string{"--stdio"},
		Enabled: true,
	}
	AutoPopulateOverrides(def)

	if def.Overrides != nil && len(def.Overrides) != 0 {
		t.Errorf("expected no overrides for direct binary, got %v", def.Overrides)
	}
}

func TestResolveForAgent(t *testing.T) {
	def := &McpServerDef{
		Command: "uvx",
		Args:    []string{"mcp-server-github"},
		Enabled: true,
		Overrides: map[string]*McpServerOverride{
			"codex": {Command: "npx", Args: []string{"-y", "@modelcontextprotocol/server-github"}},
		},
	}

	cmd, args := def.ResolveForAgent("claude")
	if cmd != "uvx" {
		t.Errorf("claude: expected uvx, got %q", cmd)
	}
	if len(args) != 1 || args[0] != "mcp-server-github" {
		t.Errorf("claude: unexpected args %v", args)
	}

	cmd, args = def.ResolveForAgent("codex")
	if cmd != "npx" {
		t.Errorf("codex: expected npx, got %q", cmd)
	}
	if len(args) < 2 || args[1] != "@modelcontextprotocol/server-github" {
		t.Errorf("codex: unexpected args %v", args)
	}
}

func TestResolvedServersForAgent_ExcludesUvxOnly(t *testing.T) {
	mgr := NewManager()
	mgr.AddServer("fetch", &McpServerDef{
		Command: "uvx",
		Args:    []string{"mcp-server-fetch"},
		Enabled: true,
	})
	mgr.AddServer("github", &McpServerDef{
		Command: "uvx",
		Args:    []string{"mcp-server-github"},
		Enabled: true,
	})

	// Claude gets both.
	claude := mgr.resolvedServersForAgent("claude")
	if _, ok := claude["fetch"]; !ok {
		t.Error("claude: fetch should be present")
	}
	if _, ok := claude["github"]; !ok {
		t.Error("claude: github should be present")
	}

	// Codex: fetch excluded (uvx-only), github translated to npx.
	codex := mgr.resolvedServersForAgent("codex")
	if _, ok := codex["fetch"]; ok {
		t.Error("codex: fetch should be excluded (uvx-only)")
	}
	if s, ok := codex["github"]; ok {
		if s.Command != "npx" {
			t.Errorf("codex: github expected npx, got %q", s.Command)
		}
	} else {
		t.Error("codex: github should be present with npx override")
	}
}
