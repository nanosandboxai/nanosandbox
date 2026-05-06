package mcp

import (
	"fmt"
	"log"
	"os"
	"regexp"
	"sync"

	"gopkg.in/yaml.v3"
)

var envVarPattern = regexp.MustCompile(`\$\{([^}]+)\}`)

// Manager handles MCP server configuration and per-agent config generation.
type Manager struct {
	mu        sync.RWMutex
	config    McpConfig
	agentType string // when set, only generate for this agent type
}

// NewManager creates an empty Manager with built-in agent config paths.
// No default MCP servers — all servers are user-defined at runtime.
func NewManager() *Manager {
	return &Manager{
		config: McpConfig{
			Version: "1",
			Defaults: McpDefaults{
				TimeoutSec:        30,
				StartupTimeoutSec: 10,
			},
			Servers: make(map[string]*McpServerDef),
			Agents: map[string]*AgentMcpConfig{
				"claude": {ConfigPath: "/home/developer/.claude.json", Format: "claude"},
				"goose":  {ConfigPath: "/home/developer/.config/goose/config.yaml", Format: "goose"},
				"codex":  {ConfigPath: "/home/developer/.codex/config.toml", Format: "codex"},
				"cursor": {ConfigPath: "/home/developer/.cursor/mcp.json", Format: "cursor"},
			},
		},
	}
}

// NewManagerFromBytes creates a Manager from raw YAML bytes.
func NewManagerFromBytes(data []byte) (*Manager, error) {
	var cfg McpConfig
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("parsing mcp config: %w", err)
	}

	if cfg.Servers == nil {
		cfg.Servers = make(map[string]*McpServerDef)
	}
	if cfg.Agents == nil {
		cfg.Agents = make(map[string]*AgentMcpConfig)
	}

	m := &Manager{config: cfg}
	m.resolveEnvVars()
	return m, nil
}

// resolveEnvVars replaces ${VAR} references in server env values with
// actual environment variable values. Unresolvable vars become empty string.
func (m *Manager) resolveEnvVars() {
	for _, srv := range m.config.Servers {
		if srv.Env == nil {
			continue
		}
		for k, v := range srv.Env {
			srv.Env[k] = envVarPattern.ReplaceAllStringFunc(v, func(match string) string {
				varName := envVarPattern.FindStringSubmatch(match)[1]
				val, ok := os.LookupEnv(varName)
				if !ok {
					log.Printf("[mcp] WARNING: env var %s not set for server env key %s", varName, k)
					return ""
				}
				return val
			})
		}
	}
}

// SetAgentType sets the active agent type. When set, GenerateAllConfigs
// only generates config files for this agent type instead of all types.
func (m *Manager) SetAgentType(agentType string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.agentType = agentType
	log.Printf("[mcp] agent type set to %q", agentType)
}

// AgentType returns the currently configured agent type (empty string if not set).
func (m *Manager) AgentType() string {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.agentType
}

// ListServers returns a deep copy of all configured server definitions.
func (m *Manager) ListServers() map[string]*McpServerDef {
	m.mu.RLock()
	defer m.mu.RUnlock()

	result := make(map[string]*McpServerDef, len(m.config.Servers))
	for k, v := range m.config.Servers {
		cp := *v
		result[k] = &cp
	}
	return result
}

// GetServer returns a copy of a server definition by name, or nil if not found.
func (m *Manager) GetServer(name string) *McpServerDef {
	m.mu.RLock()
	defer m.mu.RUnlock()
	srv, ok := m.config.Servers[name]
	if !ok {
		return nil
	}
	cp := *srv
	return &cp
}

// ResetConfig clears only config-sourced MCP servers.
// Runtime-added servers (from TUI/API) are preserved across bootstrap.
func (m *Manager) ResetConfig() {
	m.mu.Lock()
	defer m.mu.Unlock()
	for name, def := range m.config.Servers {
		if def.Source != "runtime" {
			delete(m.config.Servers, name)
		}
	}
	log.Println("[mcp] reset config-sourced servers")
}

// AddServer registers a new MCP server definition.
// Automatically populates per-agent command overrides for cross-agent compatibility.
func (m *Manager) AddServer(name string, def *McpServerDef) {
	AutoPopulateOverrides(def)
	m.mu.Lock()
	defer m.mu.Unlock()
	m.config.Servers[name] = def
	if len(def.Overrides) > 0 {
		agents := make([]string, 0, len(def.Overrides))
		for a := range def.Overrides {
			agents = append(agents, a)
		}
		log.Printf("[mcp] added server %q (overrides for: %v)", name, agents)
	} else {
		log.Printf("[mcp] added server %q", name)
	}
}

// RemoveServer removes an MCP server by name.
func (m *Manager) RemoveServer(name string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	delete(m.config.Servers, name)
	log.Printf("[mcp] removed server %q", name)
}

// EnableServer sets a server's enabled flag to true.
func (m *Manager) EnableServer(name string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if srv, ok := m.config.Servers[name]; ok {
		srv.Enabled = true
		log.Printf("[mcp] enabled server %q", name)
	}
}

// DisableServer sets a server's enabled flag to false.
func (m *Manager) DisableServer(name string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if srv, ok := m.config.Servers[name]; ok {
		srv.Enabled = false
		log.Printf("[mcp] disabled server %q", name)
	}
}

// EnabledServersForAgent returns the servers that should be configured for
// a given agent, filtering out disabled servers and excluded ones.
func (m *Manager) EnabledServersForAgent(agentName string) map[string]*McpServerDef {
	m.mu.RLock()
	defer m.mu.RUnlock()

	agentCfg, ok := m.config.Agents[agentName]
	if !ok {
		result := make(map[string]*McpServerDef)
		for k, v := range m.config.Servers {
			if v.Enabled {
				result[k] = v
			}
		}
		return result
	}

	excludeSet := make(map[string]bool, len(agentCfg.Exclude))
	for _, e := range agentCfg.Exclude {
		excludeSet[e] = true
	}

	result := make(map[string]*McpServerDef)
	for name, srv := range m.config.Servers {
		if srv.Enabled && !excludeSet[name] {
			result[name] = srv
		}
	}
	return result
}

// AgentConfig returns the agent-specific configuration, or nil.
func (m *Manager) AgentConfig(agentName string) *AgentMcpConfig {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.config.Agents[agentName]
}

// Config returns the full parsed config (for serialization to API responses).
func (m *Manager) Config() McpConfig {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.config
}

// ServerAgentStatus describes what a specific agent receives for a server.
type ServerAgentStatus struct {
	Active   bool   `json:"active"`
	Command  string `json:"command,omitempty"`
	Excluded bool   `json:"excluded,omitempty"`
	Reason   string `json:"reason,omitempty"`
}

// ServerCompatInfo returns per-agent compatibility info for a named server.
func (m *Manager) ServerCompatInfo(name string) map[string]*ServerAgentStatus {
	m.mu.RLock()
	defer m.mu.RUnlock()

	srv, ok := m.config.Servers[name]
	if !ok {
		return nil
	}

	result := make(map[string]*ServerAgentStatus)
	for agentName := range m.config.Agents {
		cmd, _ := srv.ResolveForAgent(agentName)
		status := &ServerAgentStatus{Active: srv.Enabled, Command: cmd}

		if srv.Overrides != nil {
			if ov, ok := srv.Overrides[agentName]; ok {
				if ov.Excluded {
					status.Active = false
					status.Excluded = true
					status.Command = ""
					status.Reason = fmt.Sprintf("no %s-compatible package for %q", agentName, srv.Command)
				} else {
					status.Reason = fmt.Sprintf("translated from %s to %s", srv.Command, ov.Command)
				}
			}
		}
		result[agentName] = status
	}
	return result
}

// AddServerRaw registers a server without running AutoPopulateOverrides.
// Used when loading persisted state that already has overrides.
func (m *Manager) AddServerRaw(name string, def *McpServerDef) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.config.Servers[name] = def
	log.Printf("[mcp] loaded server %q (raw)", name)
}
