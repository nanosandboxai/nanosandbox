package skills

import (
	"log"
	"sync"
)

// agentConfigs defines where each agent stores skills and prompts.
var agentConfigs = map[string]*AgentSkillConfig{
	"claude": {
		Format:     "claude",
		SkillsDir:  "/home/developer/.claude/skills",                       // ~/.claude/skills/<name>/SKILL.md
		PromptFile: "/home/developer/.claude/CLAUDE.md",                    // Claude reads CLAUDE.md as system instructions
	},
	"goose": {
		Format:     "goose",
		SkillsDir:  "",                                                     // Goose has no native skill files — concatenated into .goosehints
		PromptFile: "/home/developer/.config/goose/.goosehints",            // .goosehints appended to system prompt
	},
	"codex": {
		Format:     "codex",
		SkillsDir:  "/home/developer/.agents/skills",                       // ~/.agents/skills/<name>/SKILL.md (Codex native discovery)
		PromptFile: "/home/developer/.codex/AGENTS.md",                     // AGENTS.md read as global instructions
	},
	"cursor": {
		Format:     "cursor",
		SkillsDir:  "/home/developer/.cursor/rules",                        // ~/.cursor/rules/<name>.mdc (Cursor native rules discovery)
		PromptFile: "/home/developer/.cursor/rules/nanosandbox-agent.mdc",  // alwaysApply rule for sandbox preamble
	},
}

// Manager handles skill storage and per-agent file generation.
type Manager struct {
	mu          sync.RWMutex
	skills      map[string]*SkillDef
	agentPrompt string // current agent definition prompt
	agentName   string // current agent definition name
	agentType   string // "claude", "codex", "goose", "cursor" — when set, only generate for this type
}

// NewManager creates an empty skills manager.
func NewManager() *Manager {
	return &Manager{
		skills: make(map[string]*SkillDef),
	}
}

// ResetConfig clears only config-sourced skills and the agent definition.
// Runtime-added skills (from TUI/API) are preserved across bootstrap.
func (m *Manager) ResetConfig() {
	m.mu.Lock()
	defer m.mu.Unlock()
	for name, def := range m.skills {
		if def.Source != SourceRuntime {
			delete(m.skills, name)
		}
	}
	m.agentName = ""
	m.agentPrompt = ""
	log.Println("[skills] reset config-sourced skills and agent definition")
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

// SetAgentType sets the active agent type. When set, GenerateAllConfigs
// only generates config files for this agent type instead of all types.
func (m *Manager) SetAgentType(agentType string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.agentType = agentType
	log.Printf("[skills] agent type set to %q", agentType)
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
