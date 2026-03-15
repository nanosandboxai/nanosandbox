package skills

import (
	"log"
	"sync"
)

// agentConfigs defines where each agent stores skills and prompts.
var agentConfigs = map[string]*AgentSkillConfig{
	"claude": {
		Format:     "claude",
		SkillsDir:  "/workspace/.claude/skills",
		PromptFile: "/workspace/CLAUDE.md",
	},
	"goose": {
		Format:     "goose",
		SkillsDir:  "", // Goose uses .goosehints, not SKILL.md files
		PromptFile: "/workspace/.goosehints",
	},
	"codex": {
		Format:     "codex",
		SkillsDir:  "/workspace/.agents/skills",
		PromptFile: "/workspace/AGENTS.md",
	},
	"cursor": {
		Format:     "cursor",
		SkillsDir:  "/workspace/.cursor/skills",
		PromptFile: "/workspace/.cursor/rules/nanosandbox-agent.mdc",
	},
}

// Manager handles skill storage and per-agent file generation.
type Manager struct {
	mu          sync.RWMutex
	skills      map[string]*SkillDef
	agentPrompt string // current agent definition prompt
	agentName   string // current agent definition name
}

// NewManager creates an empty skills manager.
func NewManager() *Manager {
	return &Manager{
		skills: make(map[string]*SkillDef),
	}
}

// AddSkill registers or updates a skill definition.
func (m *Manager) AddSkill(name string, def *SkillDef) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.skills[name] = def
	log.Printf("[skills] added skill %q", name)
}

// RemoveSkill removes a skill by name.
func (m *Manager) RemoveSkill(name string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	delete(m.skills, name)
	log.Printf("[skills] removed skill %q", name)
}

// ListSkills returns a copy of all skill definitions.
func (m *Manager) ListSkills() map[string]*SkillDef {
	m.mu.RLock()
	defer m.mu.RUnlock()

	result := make(map[string]*SkillDef, len(m.skills))
	for k, v := range m.skills {
		cp := *v
		result[k] = &cp
	}
	return result
}

// GetSkill returns a copy of a skill by name, or nil if not found.
func (m *Manager) GetSkill(name string) *SkillDef {
	m.mu.RLock()
	defer m.mu.RUnlock()
	s, ok := m.skills[name]
	if !ok {
		return nil
	}
	cp := *s
	return &cp
}

// SetAgentDefinition stores the agent name and system prompt.
func (m *Manager) SetAgentDefinition(name, prompt string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.agentName = name
	m.agentPrompt = prompt
	log.Printf("[skills] set agent definition %q", name)
}

// GetAgentDefinition returns the current agent name and prompt.
func (m *Manager) GetAgentDefinition() (string, string) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return m.agentName, m.agentPrompt
}

// AgentConfigs returns the agent skill configuration map.
func AgentConfigs() map[string]*AgentSkillConfig {
	return agentConfigs
}
