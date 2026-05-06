package skills

// Source values for tracking where a skill/MCP server was defined.
const (
	SourceConfig  = "config"  // from sandbox.yml via bootstrap
	SourceRuntime = "runtime" // added at runtime via TUI / API
)

// SkillDef defines a single skill with its content.
type SkillDef struct {
	Name        string `json:"name"`
	Description string `json:"description"`
	Content     string `json:"content"`              // full markdown body
	Version     string `json:"version,omitempty"`
	Source      string `json:"source,omitempty"`      // "config" or "runtime"
	// Optional fields used when generating per-agent config files.
	WhenToUse     string   `json:"when_to_use,omitempty"`    // Claude auto-invoke hint
	AllowedTools  []string `json:"allowed_tools,omitempty"`  // Claude pre-approved tools
	UserInvocable *bool    `json:"user_invocable,omitempty"` // nil = treated as true
	Paths         []string `json:"paths,omitempty"`          // Claude glob auto-attach
}

// AgentSkillConfig defines per-agent skill/prompt generation rules.
type AgentSkillConfig struct {
	Format     string `json:"format"`      // "claude", "goose", "codex", "cursor"
	SkillsDir  string `json:"skills_dir"`  // where SKILL.md files go (empty for goose)
	PromptFile string `json:"prompt_file"` // where agent prompt goes
}
