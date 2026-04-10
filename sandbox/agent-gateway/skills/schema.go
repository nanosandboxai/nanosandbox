package skills

// SkillDef defines a single skill with its content.
type SkillDef struct {
	Name        string `json:"name"`
	Description string `json:"description"`
	Content     string `json:"content"`              // full markdown body
	Version     string `json:"version,omitempty"`
}

// AgentSkillConfig defines per-agent skill/prompt generation rules.
type AgentSkillConfig struct {
	Format     string `json:"format"`      // "claude", "goose", "codex", "cursor"
	SkillsDir  string `json:"skills_dir"`  // where SKILL.md files go (empty for goose)
	PromptFile string `json:"prompt_file"` // where agent prompt goes
}
