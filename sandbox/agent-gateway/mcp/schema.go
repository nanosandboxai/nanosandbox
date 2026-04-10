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
}

// AgentMcpConfig defines per-agent config generation rules.
type AgentMcpConfig struct {
	ConfigPath string   `yaml:"config_path" json:"config_path"`
	Format     string   `yaml:"format"      json:"format"`
	Exclude    []string `yaml:"exclude"     json:"exclude"`
}
