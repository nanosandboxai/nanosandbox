# Agents Registry

A registry of agent definitions, reusable skills, and MCP references for [nanosandbox](https://github.com/devdone-labs/nanosandbox).

## Overview

This repository provides composable building blocks for configuring AI coding agents:

- **Agent definitions** - Templates describing an agent's persona, skills, and tools (e.g., Python developer, DevOps engineer)
- **Skills** - Reusable instruction sets for specific workflows (e.g., git workflow, TDD, code review)
- **MCP references** - Pointers to Model Context Protocol servers from external registries, plus custom MCPs

## Available Agents

| Agent | Description | Skills |
|-------|-------------|--------|
| [python-developer](agents/python-developer.yaml) | Senior Python developer following PEP8 and modern best practices | git-workflow, tdd, code-review, security-best-practices |
| [rust-developer](agents/rust-developer.yaml) | Rust developer focused on safety, performance, and idiomatic code | git-workflow, tdd, code-review, security-best-practices, documentation |
| [react-developer](agents/react-developer.yaml) | React/TypeScript developer building accessible, performant UIs | git-workflow, tdd, code-review, security-best-practices |
| [devops-engineer](agents/devops-engineer.yaml) | DevOps engineer specializing in CI/CD, containers, and infrastructure | git-workflow, code-review, security-best-practices, documentation |

## Available Skills

| Skill | Description |
|-------|-------------|
| [git-workflow](skills/git-workflow.md) | Git workflow best practices with conventional commits |
| [tdd](skills/tdd.md) | Test-driven development - red, green, refactor |
| [code-review](skills/code-review.md) | Code review focused on correctness, clarity, maintainability |
| [security-best-practices](skills/security-best-practices.md) | Secure development covering OWASP top risks |
| [documentation](skills/documentation.md) | Technical documentation standards |

## Usage with nanosandbox

Reference a registry agent in your `sandbox.yml`:

```yaml
defaults:
  image: ghcr.io/devdone-labs/dd-agent-claude:latest

sandboxes:
  my-env:
    agent: python-developer
```

Or compose your own agent using registry skills:

```yaml
sandboxes:
  custom-env:
    image: ghcr.io/devdone-labs/dd-agent-claude:latest
    agent:
      skills: [git-workflow, tdd]
      mcps:
        github:
          source: smithery
          package: "@modelcontextprotocol/server-github"
```

## Discovery

The `index.json` file at the repo root provides a machine-readable catalog of all agents, skills, and MCP sources. It is regenerated automatically on every push to main.

## Agent Definition Format

Agent definitions use YAML with the following structure:

```yaml
apiVersion: v1
kind: Agent
metadata:
  name: my-agent
  description: What this agent does
  tags: [relevant, tags]

spec:
  prompt: |
    System prompt defining the agent's persona and behavior...

  skills:
    - git-workflow
    - tdd

  mcps:
    - name: server-github
      source: smithery
      package: "@modelcontextprotocol/server-github"
      env:
        GITHUB_PERSONAL_ACCESS_TOKEN: "${GITHUB_TOKEN}"
```

See `schema/agent.schema.json` for the full validation schema.

## Docker Images

This repo also contains Docker images for running agents inside nanosandbox VMs.

### Image Architecture

```
Dockerfile.base (node:22-slim + agent-gateway + MCP packages + SSH)
    |
    ├── Dockerfile.claude  → ghcr.io/nanosandboxai/agents-registry/claude
    ├── Dockerfile.codex   → ghcr.io/nanosandboxai/agents-registry/codex
    ├── Dockerfile.goose   → ghcr.io/nanosandboxai/agents-registry/goose
    └── Dockerfile.cursor  → ghcr.io/nanosandboxai/agents-registry/cursor
```

### Base Image Contents

- node:22-slim with system deps (git, SSH, curl)
- agent-gateway binary (from sandbox repo)
- Pre-installed MCP server npm packages (github, filesystem, memory, brave-search, context7)
- Non-root `developer` user (UID 1000)
- nanosb-init.sh (starts sshd + agent-gateway)

### Available Images

| Image | Agent | Registry |
|-------|-------|----------|
| claude | Claude Code | `ghcr.io/nanosandboxai/agents-registry/claude:latest` |
| codex | OpenAI Codex | `ghcr.io/nanosandboxai/agents-registry/codex:latest` |
| goose | Goose | `ghcr.io/nanosandboxai/agents-registry/goose:latest` |
| cursor | Cursor Agent | `ghcr.io/nanosandboxai/agents-registry/cursor:latest` |

### Building Locally

```bash
cd docker
docker build -f Dockerfile.base -t base:local .
docker build -f Dockerfile.claude -t claude:local .
```

## Contributing

1. Fork this repository
2. Add or modify agent definitions in `agents/`, skills in `skills/`, or custom MCPs in `mcps/custom/`
3. Validate your agent YAML against the schema in `schema/agent.schema.json`
4. Submit a pull request
