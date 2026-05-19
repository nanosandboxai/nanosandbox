package mcp

import (
	"encoding/json"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

// GenerateAllConfigs writes config files for configured agents.
// When an agent type is set, only generates for that type; otherwise generates for all.
func (m *Manager) GenerateAllConfigs() error {
	m.mu.RLock()
	defer m.mu.RUnlock()

	// If agent type is set, only generate for that specific type.
	if m.agentType != "" {
		agentCfg, ok := m.config.Agents[m.agentType]
		if ok {
			return m.generateForAgent(m.agentType, agentCfg)
		}
		log.Printf("[mcp] unknown agent type %q, generating for all agents", m.agentType)
	}

	for agentName, agentCfg := range m.config.Agents {
		if err := m.generateForAgent(agentName, agentCfg); err != nil {
			return err
		}
	}

	return nil
}

func (m *Manager) generateForAgent(agentName string, agentCfg *AgentMcpConfig) error {
	servers := m.resolvedServersForAgent(agentName)

	dir := filepath.Dir(agentCfg.ConfigPath)
	if err := os.MkdirAll(dir, 0755); err != nil {
		return fmt.Errorf("creating dir %s: %w", dir, err)
	}

	// Claude Code reads MCP from ~/.claude/settings.json which also holds hooks,
	// permissions, and other settings. Merge only the mcpServers key to avoid
	// overwriting the rest of the file.
	if agentCfg.Format == "claude" {
		if err := mergeClaudeSettings(agentCfg.ConfigPath, servers); err != nil {
			return fmt.Errorf("writing %s: %w", agentCfg.ConfigPath, err)
		}
		log.Printf("[mcp] wrote %s config: %s (%d servers)", agentName, agentCfg.ConfigPath, len(servers))
		return nil
	}

	var data []byte
	var err error

	switch agentCfg.Format {
	case "goose":
		var provider *GooseProviderConfig
		if m.gooseProvider != nil {
			cp := *m.gooseProvider
			provider = &cp
		}
		data, err = GenerateGooseConfigWithProvider(servers, provider)
	case "codex":
		data, err = GenerateCodexConfig(servers)
	case "cursor":
		data, err = GenerateCursorConfig(servers)
	default:
		log.Printf("[mcp] unknown format %q for agent %s, skipping", agentCfg.Format, agentName)
		return nil
	}

	if err != nil {
		return fmt.Errorf("generating config for %s: %w", agentName, err)
	}

	if err := os.WriteFile(agentCfg.ConfigPath, data, 0644); err != nil {
		return fmt.Errorf("writing %s: %w", agentCfg.ConfigPath, err)
	}

	log.Printf("[mcp] wrote %s config: %s (%d servers)", agentName, agentCfg.ConfigPath, len(servers))
	return nil
}

// mergeClaudeSettings reads the existing Claude settings.json (if any), replaces
// the mcpServers key, and writes the file back — preserving hooks, permissions,
// and all other settings Claude Code manages in this file.
func mergeClaudeSettings(path string, servers map[string]*McpServerDef) error {
	existing := make(map[string]interface{})

	if data, err := os.ReadFile(path); err == nil {
		// Best-effort unmarshal — if it fails we start fresh rather than abort.
		_ = json.Unmarshal(data, &existing)
	}

	// Build the mcpServers object.
	entries := make(map[string]claudeServerEntry, len(servers))
	for name, srv := range servers {
		entries[name] = claudeServerEntry{
			Command: srv.Command,
			Args:    srv.Args,
			Env:     nonEmptyEnv(srv.Env),
		}
	}
	existing["mcpServers"] = entries

	data, err := json.MarshalIndent(existing, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, data, 0644)
}

// enabledServersForAgentLocked is the internal version without locking.
func (m *Manager) enabledServersForAgentLocked(agentName string) map[string]*McpServerDef {
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

// --- Claude Code Format ---

type claudeServerEntry struct {
	Command string            `json:"command"`
	Args    []string          `json:"args"`
	Env     map[string]string `json:"env,omitempty"`
}

func GenerateClaudeConfig(servers map[string]*McpServerDef) ([]byte, error) {
	entries := make(map[string]claudeServerEntry, len(servers))
	for name, srv := range servers {
		entries[name] = claudeServerEntry{
			Command: srv.Command,
			Args:    srv.Args,
			Env:     nonEmptyEnv(srv.Env),
		}
	}
	wrapper := map[string]interface{}{"mcpServers": entries}
	return json.MarshalIndent(wrapper, "", "  ")
}

// --- Goose Format ---

// GooseProviderConfig holds optional provider/model settings to write into
// the goose config.yaml alongside MCP extensions. When empty, the provider
// section is omitted and goose falls back to its own config or env vars.
type GooseProviderConfig struct {
	Provider string // e.g. "anthropic", "openai", "google-gemini"
	Model    string // e.g. "claude-sonnet-4-5-20250929"
}

func GenerateGooseConfig(servers map[string]*McpServerDef) ([]byte, error) {
	return GenerateGooseConfigWithProvider(servers, nil)
}

func GenerateGooseConfigWithProvider(servers map[string]*McpServerDef, provider *GooseProviderConfig) ([]byte, error) {
	var b strings.Builder

	// Write provider/model settings if provided.
	if provider != nil && provider.Provider != "" {
		b.WriteString(fmt.Sprintf("GOOSE_PROVIDER: %q\n", provider.Provider))
		if provider.Model != "" {
			b.WriteString(fmt.Sprintf("GOOSE_MODEL: %q\n", provider.Model))
		}
		b.WriteString("\n")
	}

	b.WriteString("extensions:\n")

	names := sortedKeys(servers)
	for _, name := range names {
		srv := servers[name]
		b.WriteString(fmt.Sprintf("  %s:\n", name))
		b.WriteString(fmt.Sprintf("    name: %s\n", name))
		b.WriteString(fmt.Sprintf("    cmd: %s\n", srv.Command))

		if len(srv.Args) > 0 {
			b.WriteString("    args:\n")
			for _, arg := range srv.Args {
				b.WriteString(fmt.Sprintf("      - %q\n", arg))
			}
		}

		env := nonEmptyEnv(srv.Env)
		if len(env) > 0 {
			b.WriteString("    envs:\n")
			envKeys := sortedMapKeys(env)
			for _, k := range envKeys {
				b.WriteString(fmt.Sprintf("      %s: %q\n", k, env[k]))
			}
		}

		b.WriteString("    type: stdio\n")
		b.WriteString("    enabled: true\n")
		b.WriteString("    timeout: 300\n")
	}

	return []byte(b.String()), nil
}

// --- Codex Format ---

func GenerateCodexConfig(servers map[string]*McpServerDef) ([]byte, error) {
	var b strings.Builder

	names := sortedKeys(servers)
	for i, name := range names {
		srv := servers[name]
		if i > 0 {
			b.WriteString("\n")
		}
		b.WriteString(fmt.Sprintf("[mcp_servers.%s]\n", name))
		b.WriteString(fmt.Sprintf("command = %q\n", srv.Command))

		b.WriteString("args = [")
		for j, arg := range srv.Args {
			if j > 0 {
				b.WriteString(", ")
			}
			b.WriteString(fmt.Sprintf("%q", arg))
		}
		b.WriteString("]\n")

		env := nonEmptyEnv(srv.Env)
		if len(env) > 0 {
			b.WriteString("env = { ")
			envKeys := sortedMapKeys(env)
			for j, k := range envKeys {
				if j > 0 {
					b.WriteString(", ")
				}
				b.WriteString(fmt.Sprintf("%q = %q", k, env[k]))
			}
			b.WriteString(" }\n")
		}
	}

	return []byte(b.String()), nil
}

// --- Cursor Format (same as Claude) ---

func GenerateCursorConfig(servers map[string]*McpServerDef) ([]byte, error) {
	return GenerateClaudeConfig(servers)
}

// resolvedServersForAgent returns enabled servers for an agent with per-agent
// overrides applied. Excluded servers are omitted. Each returned McpServerDef
// is a copy with the correct command/args for that specific agent.
func (m *Manager) resolvedServersForAgent(agentName string) map[string]*McpServerDef {
	base := m.enabledServersForAgentLocked(agentName)
	result := make(map[string]*McpServerDef, len(base))
	for name, srv := range base {
		// Check for per-agent exclusion (e.g. uvx-only package on codex).
		if srv.Overrides != nil {
			if ov, ok := srv.Overrides[agentName]; ok && ov.Excluded {
				log.Printf("[mcp] skipping %q for %s (excluded: no compatible runtime)", name, agentName)
				continue
			}
		}
		cmd, args := srv.ResolveForAgent(agentName)
		result[name] = &McpServerDef{
			Command: cmd,
			Args:    args,
			Env:     srv.Env,
			Enabled: srv.Enabled,
		}
	}
	return result
}

// --- Helpers ---

func nonEmptyEnv(env map[string]string) map[string]string {
	if len(env) == 0 {
		return nil
	}
	result := make(map[string]string)
	for k, v := range env {
		if v != "" {
			result[k] = v
		}
	}
	if len(result) == 0 {
		return nil
	}
	return result
}

func sortedKeys(m map[string]*McpServerDef) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}

func sortedMapKeys(m map[string]string) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}
