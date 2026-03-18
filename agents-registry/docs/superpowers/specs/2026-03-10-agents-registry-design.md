# Agents Registry Design

## Overview

A definition-only registry for agent definitions, skills, and MCP references. Provides a catalog of reusable, composable agent templates that nanosandbox consumes to configure sandboxed AI coding agents.

**Scope:** Agent definitions + skills + MCP references (images + infrastructure stay in nanosandbox).

## Key Design Decisions

1. **Hybrid flat-file + generated index** - YAML/markdown files as source of truth, with a generated `index.json` for lightweight discovery
2. **Generic, agent-agnostic formats** - Registry stores generic definitions; the agent-gateway in nanosandbox translates to each agent's native format (Claude JSON, Goose YAML, Codex TOML, etc.)
3. **External MCP references only** - No MCP packages stored in the registry. Custom MCPs we build live under `mcps/custom/`; all others are references to external registries (Smithery, official MCP repos, mcp.run)
4. **npx on-demand installation** - MCP packages are installed at VM startup via npx, not baked into images. The agent-gateway drives installation and streams progress
5. **Gateway as universal translator** - The agent-gateway acts as a proxy for agents that don't natively support MCPs, translating capabilities through whatever mechanism the agent supports

## Repository Structure

```
agents-registry/
├── agents/                          # Agent definitions
│   ├── python-developer.yaml
│   ├── rust-developer.yaml
│   ├── react-developer.yaml
│   └── devops-engineer.yaml
├── skills/                          # Reusable skill definitions
│   ├── git-workflow.md
│   ├── tdd.md
│   ├── code-review.md
│   ├── security-best-practices.md
│   └── documentation.md
├── mcps/
│   ├── custom/                      # Custom MCPs we build
│   │   └── (future custom MCPs)
│   └── sources.yaml                 # External registry references
├── schema/
│   └── agent.schema.json            # JSON Schema for validation
├── .github/
│   └── workflows/
│       └── build-index.yaml         # Generates index.json on push
├── index.json                       # Auto-generated catalog
└── README.md
```

## Agent Definition Format

```yaml
# agents/python-developer.yaml
apiVersion: v1
kind: Agent
metadata:
  name: python-developer
  description: Senior Python developer following modern best practices
  tags: [python, backend, testing]

spec:
  prompt: |
    You are a senior Python developer. You follow PEP8 conventions,
    write comprehensive tests, use type hints, and prefer simple,
    readable solutions over clever ones. You use poetry or uv for
    dependency management and pytest for testing.

  skills:
    - git-workflow
    - tdd
    - code-review

  mcps:
    # External MCPs - referenced from registries defined in sources.yaml
    - name: server-github
      source: smithery
      package: "@modelcontextprotocol/server-github"
      env:
        GITHUB_PERSONAL_ACCESS_TOKEN: "${GITHUB_TOKEN}"
    - name: server-filesystem
      source: official
      package: "@modelcontextprotocol/server-filesystem"

    # Custom MCPs from this repo
    - name: project-scaffold
      source: custom
      path: mcps/custom/project-scaffold
```

## Skill Format

Skills use markdown with YAML frontmatter:

```markdown
---
name: git-workflow
description: Git workflow best practices with conventional commits
version: "1.0"
tags: [git, workflow, collaboration]
---

# Git Workflow

## Commit Messages
Follow conventional commits format...
```

The frontmatter provides metadata for the index. The markdown body contains the actual instructions that get translated by the agent-gateway into whatever format each agent understands.

## External MCP Sources

```yaml
# mcps/sources.yaml
apiVersion: v1
kind: McpSources

registries:
  smithery:
    url: https://registry.smithery.ai
    type: smithery
    description: Smithery MCP registry

  official:
    url: https://github.com/modelcontextprotocol/servers
    type: github
    description: Official MCP server repository

  mcp-run:
    url: https://www.mcp.run
    type: mcp-run
    description: Hosted MCP servers
```

## Generated Index

GitHub Actions generates `index.json` on push to main, scanning all YAML/markdown files:

```json
{
  "version": "1.0",
  "updated_at": "2026-03-10T12:00:00Z",
  "registry_url": "https://github.com/devdone-labs/agents-registry",
  "agents": [
    {
      "name": "python-developer",
      "description": "Senior Python developer following modern best practices",
      "tags": ["python", "backend", "testing"],
      "skills": ["git-workflow", "tdd", "code-review"],
      "mcps": ["server-github", "server-filesystem"],
      "path": "agents/python-developer.yaml"
    }
  ],
  "skills": [
    {
      "name": "git-workflow",
      "description": "Git workflow best practices with conventional commits",
      "tags": ["git", "workflow"],
      "path": "skills/git-workflow.md"
    }
  ],
  "mcps": {
    "custom": [],
    "sources": ["smithery", "official", "mcp-run"]
  }
}
```

## Consumption Flow (nanosandbox integration - separate effort)

Users reference registry agents in their `sandbox.yml`:

```yaml
# sandbox.yml (in user's project)
defaults:
  image: ghcr.io/devdone-labs/dd-agent-claude:latest

sandboxes:
  my-python-env:
    agent: python-developer          # from registry
    mcp:                             # user can override/extend
      memory:
        command: npx
        args: ["-y", "@modelcontextprotocol/server-memory"]
        enabled: true
```

SDK flow:
1. Reads `sandbox.yml`, sees `agent: python-developer`
2. Fetches `index.json` from registry (or uses cached version)
3. Fetches `agents/python-developer.yaml` + referenced skills
4. Merges: registry agent definition < sandbox.yml overrides < CLI flags
5. Passes merged config to agent-gateway
6. Gateway installs MCPs via npx, translates skills, applies prompt, streams progress
7. Agent session starts

## MCP Installation at VM Startup

The agent-gateway drives MCP installation on boot:

1. Gateway starts as PID 1
2. Receives MCP definitions from SDK
3. Installs each MCP package via `npx -y <package>`
4. Streams progress via SSE endpoint (`GET /api/v1/startup/status`)
5. Writes agent-specific config files
6. Reports healthy via `GET /health`

User sees:
```
[nanosb] Starting sandbox...
[nanosb] Installing MCP: @modelcontextprotocol/server-github... done
[nanosb] Installing MCP: @upstash/context7-mcp... done
[nanosb] Configuring agent: claude... done
[nanosb] Ready.
```

## Out of Scope (for now)

- Dockerfiles and agent image builds (stay in nanosandbox)
- Agent-gateway source code (stays in nanosandbox)
- Skills/templates as a third registry layer (future)
- Versioning and dependency resolution (future, if needed)
