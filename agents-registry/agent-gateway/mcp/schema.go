package mcp

// McpConfig is the root configuration for MCP server management.
type McpConfig struct {
	Version  string                     `yaml:"version"  json:"version"`
	Defaults McpDefaults                `yaml:"defaults" json:"defaults"`
	Servers  map[string]*McpServerDef   `yaml:"servers"  json:"servers"`
	Agents   map[string]*AgentMcpConfig `yaml:"agents"   json:"agents"`
}

// McpDefaults contains default settings applied to all servers.
type McpDefaults struct {
	TimeoutSec        int `yaml:"timeout_sec"         json:"timeout_sec"`
	StartupTimeoutSec int `yaml:"startup_timeout_sec"  json:"startup_timeout_sec"`
}

// McpServerDef defines a single MCP server.
type McpServerDef struct {
	Command string            `yaml:"command" json:"command"`
	Args    []string          `yaml:"args"    json:"args"`
	Env     map[string]string `yaml:"env"     json:"env,omitempty"`
	Enabled bool              `yaml:"enabled" json:"enabled"`
	Source  string            `yaml:"source,omitempty"  json:"source,omitempty"` // "config" or "runtime"
	// Per-agent command overrides. When generating config for an agent,
	// if an override exists the agent gets that command+args instead of
	// the top-level ones. This lets a single /mcp add --all work across
	// agents that need different package managers (uvx vs npx).
	Overrides map[string]*McpServerOverride `yaml:"overrides,omitempty" json:"overrides,omitempty"`
}

// McpServerOverride replaces Command and/or Args for a specific agent.
// If Excluded is true, the server is skipped entirely for that agent.
type McpServerOverride struct {
	Command  string   `yaml:"command" json:"command"`
	Args     []string `yaml:"args"    json:"args"`
	Excluded bool     `yaml:"excluded,omitempty" json:"excluded,omitempty"`
}

// ResolveForAgent returns the command and args to use for a given agent.
// If an override exists for that agent, it is used; otherwise the defaults.
func (d *McpServerDef) ResolveForAgent(agentName string) (string, []string) {
	if d.Overrides != nil {
		if ov, ok := d.Overrides[agentName]; ok {
			return ov.Command, ov.Args
		}
	}
	return d.Command, d.Args
}

// AgentMcpConfig defines per-agent config generation rules.
type AgentMcpConfig struct {
	ConfigPath string   `yaml:"config_path" json:"config_path"`
	Format     string   `yaml:"format"      json:"format"`
	Exclude    []string `yaml:"exclude"     json:"exclude"`
}
