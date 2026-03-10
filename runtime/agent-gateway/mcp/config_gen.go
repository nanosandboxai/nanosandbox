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

// GenerateAllConfigs writes config files for all configured agents.
func (m *Manager) GenerateAllConfigs() error {
	m.mu.RLock()
	defer m.mu.RUnlock()

	for agentName, agentCfg := range m.config.Agents {
		servers := m.enabledServersForAgentLocked(agentName)

		var data []byte
		var err error

		switch agentCfg.Format {
		case "claude":
			data, err = GenerateClaudeConfig(servers)
		case "goose":
			data, err = GenerateGooseConfig(servers)
		case "codex":
			data, err = GenerateCodexConfig(servers)
		case "cursor":
			data, err = GenerateCursorConfig(servers)
		default:
			log.Printf("[mcp] unknown format %q for agent %s, skipping", agentCfg.Format, agentName)
			continue
		}

		if err != nil {
			return fmt.Errorf("generating config for %s: %w", agentName, err)
		}

		dir := filepath.Dir(agentCfg.ConfigPath)
		if err := os.MkdirAll(dir, 0755); err != nil {
			return fmt.Errorf("creating dir %s: %w", dir, err)
		}

		if err := os.WriteFile(agentCfg.ConfigPath, data, 0644); err != nil {
			return fmt.Errorf("writing %s: %w", agentCfg.ConfigPath, err)
		}

		log.Printf("[mcp] wrote %s config: %s (%d servers)", agentName, agentCfg.ConfigPath, len(servers))
	}

	return nil
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

func GenerateGooseConfig(servers map[string]*McpServerDef) ([]byte, error) {
	var b strings.Builder
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
