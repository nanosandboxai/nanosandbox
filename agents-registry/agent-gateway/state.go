package main

import (
	"encoding/json"
	"errors"
	"log"
	"os"
	"path/filepath"

	"github.com/nanosandboxai/agent-gateway/mcp"
	"github.com/nanosandboxai/agent-gateway/skills"
)

const gatewayStateFile = "/home/developer/.nanosandbox/state.json"

// gatewayState is the on-disk representation of the gateway's mutable state.
type gatewayState struct {
	Version     int                          `json:"version"`
	AgentName   string                       `json:"agent_name"`
	AgentPrompt string                       `json:"agent_prompt"`
	AgentType   string                       `json:"agent_type"`
	Skills      map[string]*skills.SkillDef  `json:"skills"`
	McpServers  map[string]*mcp.McpServerDef `json:"mcp_servers"`
}

// saveGatewayState writes current skill + MCP state to disk.
func saveGatewayState(mcpMgr *mcp.Manager, skillsMgr *skills.Manager, path string) error {
	agentName, agentPrompt := skillsMgr.GetAgentDefinition()
	state := gatewayState{
		Version:     1,
		AgentName:   agentName,
		AgentPrompt: agentPrompt,
		AgentType:   mcpMgr.AgentType(),
		Skills:      skillsMgr.ListSkills(),
		McpServers:  mcpMgr.ListServers(),
	}

	data, err := json.MarshalIndent(state, "", "  ")
	if err != nil {
		return err
	}

	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		return err
	}

	return os.WriteFile(path, data, 0644)
}

// loadGatewayState reads the state file and populates both managers.
// Returns nil if the file does not exist (first boot).
func loadGatewayState(mcpMgr *mcp.Manager, skillsMgr *skills.Manager, path string) error {
	data, err := os.ReadFile(path)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil
		}
		return err
	}

	var state gatewayState
	if err := json.Unmarshal(data, &state); err != nil {
		return err
	}

	for name, def := range state.Skills {
		skillsMgr.AddSkill(name, def)
	}
	for name, def := range state.McpServers {
		mcpMgr.AddServerRaw(name, def) // Raw: preserves persisted overrides without re-computing
	}
	if state.AgentName != "" {
		skillsMgr.SetAgentDefinition(state.AgentName, state.AgentPrompt)
	}
	if state.AgentType != "" {
		mcpMgr.SetAgentType(state.AgentType)
		skillsMgr.SetAgentType(state.AgentType)
	}

	log.Printf("[agent-gateway] loaded state: %d skills, %d MCP servers, agent=%q",
		len(state.Skills), len(state.McpServers), state.AgentName)
	return nil
}
