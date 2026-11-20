# Nanosandbox sample sandboxes

A grab-bag of runnable `sandbox.yml` definitions showing different code agents
combined with a variety of **skills**, **MCP servers**, permissions, models and
prompts. Use it to try the `nanosb` TUI yourself.

## Files

| File | Purpose |
|------|---------|
| `sandbox.yml` | 10 sandboxes across all 8 agent types with different skill/MCP combos |
| `sample.env` | API keys / tokens, loaded by the sandboxes via `env_file:` (edit it first) |
| `data/` | host directory for the read-only mount example (`claude-net-mounts`) |

## Run it

```bash
cd examples/sandboxes

# 1. Put real keys in sample.env (ANTHROPIC_API_KEY, OPENAI_API_KEY, ...)

# 2. Validate without booting anything:
nanosb apply --dry-run

# 3. Launch the TUI from this directory (auto-detects ./sandbox.yml):
nanosb

# ...or create every sandbox and attach:
nanosb apply
nanosb ps

# ...or run a single sandbox directly:
nanosb --sandbox claude-fullstack
```

In the TUI, inspect what each sandbox resolved with the slash commands:

```
/skills list          # skills configured in sandbox.yml
/mcp list             # MCP servers configured in sandbox.yml
/agent list           # agents available from the registry
/agent show <name>    # details for an agent
```

## What each sandbox demonstrates

| Sandbox | Agent type | Skills | MCP servers | Notes |
|---------|-----------|--------|-------------|-------|
| `claude-fullstack` | claude | git-commit, test-driven-development, requesting-code-review | github, memory | accept-edits + prompt |
| `claude-interactive` | claude | systematic-debugging, verification-before-completion | filesystem | interactive, dark theme |
| `codex-autorefactor` | codex | writing-plans, executing-plans, spec-to-implementation | memory | auto-mode, allow-all |
| `goose-research` | goose | research-documentation, knowledge-capture, content-research-writer | fetch, filesystem | uvx + npx mix |
| `cursor-frontend` | cursor | frontend-design, webapp-testing, artifacts-builder | puppeteer | browser automation |
| `opencode-multiagent` | opencode | subagent-driven-development, dispatching-parallel-agents, multi-agent-patterns | memory, sequential-thinking | multi-MCP |
| `aider-tdd` | aider | test-driven-development, git-commit, verification-before-completion | git | uvx server with args |
| `gemini-docs` | gemini | writing-skills, research-documentation, doc-coauthoring | time | docs-focused |
| `crush-review` | crush | github-pr-review, requesting-code-review, receiving-code-review | github | needs GITHUB token |
| `claude-net-mounts` | claude | tool-design, mcp-builder | postgres | network + read-only mount |

## Notes

- Sandboxes without a `prompt` are marked `interactive: true` (you drive the agent
  from the TUI console); sandboxes with a `prompt` run headless (one-shot task).
  A headless agent with no prompt would launch with nothing to do.
- Skills are resolved from the public skills registry; MCP servers run inside the
  guest via `npx`/`uvx`, so the sandbox needs network access (default: enabled).
- `env_file:` (not `${VAR}`) is used for secrets so the file works even when the
  variables are not exported in your shell. `${VAR}` inside `sandbox.yml` reads
  the **host shell** environment only.
- Model names are validated against the built-in model catalog (overridable via
  `~/.nanosandbox/models.json` or `~/.nanosandbox/models.yaml`).
