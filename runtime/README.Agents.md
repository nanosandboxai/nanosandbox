# DD Agents Registry

Minimal Docker images containing AI coding agent CLIs for use in sandboxed development environments.

## Available Agents

| Agent | CLI Command | Description |
|-------|-------------|-------------|
| [Claude Code](https://docs.anthropic.com/claude-code) | `claude` | Anthropic's AI coding assistant |
| [Goose](https://github.com/block/goose) | `goose` | AI developer agent by Block |
| [Codex](https://github.com/openai/codex) | `codex` | OpenAI's coding assistant |
| [Cursor CLI](https://cursor.com/cli) | `cursor-agent` | Cursor's AI agent |

## Quick Start

### Linux

#### Pull an Agent Image

```bash
# Pull the agent you need
docker pull ghcr.io/devdone-labs/dd-agent-claude:latest
docker pull ghcr.io/devdone-labs/dd-agent-goose:latest
docker pull ghcr.io/devdone-labs/dd-agent-codex:latest
docker pull ghcr.io/devdone-labs/dd-agent-cursor:latest
```

#### Run an Agent

```bash
# Run Claude Code
docker run -it --rm -v $(pwd):/workspace ghcr.io/devdone-labs/dd-agent-claude:latest claude

# Run Goose
docker run -it --rm -v $(pwd):/workspace ghcr.io/devdone-labs/dd-agent-goose:latest goose

# Run Codex
docker run -it --rm -v $(pwd):/workspace ghcr.io/devdone-labs/dd-agent-codex:latest codex

# Run Cursor CLI
docker run -it --rm -v $(pwd):/workspace ghcr.io/devdone-labs/dd-agent-cursor:latest cursor-agent
```

### Windows

> **Note:** Windows containers require Windows 10/11 Pro, Enterprise, or Windows Server with the Containers feature enabled.

#### Pull an Agent Image

```powershell
docker pull ghcr.io/devdone-labs/dd-agent-claude:latest
```

#### Run an Agent

```powershell
# Run Claude Code
docker run -it --rm -v ${PWD}:C:\workspace ghcr.io/devdone-labs/dd-agent-claude:latest claude

# Run Goose
docker run -it --rm -v ${PWD}:C:\workspace ghcr.io/devdone-labs/dd-agent-goose:latest goose

# Run Codex
docker run -it --rm -v ${PWD}:C:\workspace ghcr.io/devdone-labs/dd-agent-codex:latest codex

# Run Cursor CLI
docker run -it --rm -v ${PWD}:C:\workspace ghcr.io/devdone-labs/dd-agent-cursor:latest cursor-agent
```

### With API Keys

Most agents require API keys. Pass them as environment variables:

```bash
# Linux/macOS
docker run -it --rm \
  -e ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY \
  -v $(pwd):/workspace \
  ghcr.io/devdone-labs/dd-agent-claude:latest claude
```

```powershell
# Windows
docker run -it --rm `
  -e ANTHROPIC_API_KEY=$env:ANTHROPIC_API_KEY `
  -v ${PWD}:C:\workspace `
  ghcr.io/devdone-labs/dd-agent-claude:latest claude
```

## Image Details

### Per-Agent Images

Each agent ships as its own image, built on a shared Alpine base:

- **Base:** Alpine 3.20 + Node.js 22 + agent-gateway + MCP packages
- **Platforms:** linux/amd64, linux/arm64
- **Registry pattern:** `ghcr.io/devdone-labs/dd-agent-<name>`

| Image | Registry | Est. Size |
|-------|----------|-----------|
| `dd-agents-base` | `ghcr.io/devdone-labs/dd-agents-base` | ~150 MB |
| `dd-agent-claude` | `ghcr.io/devdone-labs/dd-agent-claude` | ~200 MB |
| `dd-agent-goose` | `ghcr.io/devdone-labs/dd-agent-goose` | ~180 MB |
| `dd-agent-codex` | `ghcr.io/devdone-labs/dd-agent-codex` | ~200 MB |
| `dd-agent-cursor` | `ghcr.io/devdone-labs/dd-agent-cursor` | ~180 MB |

| Tag | Description |
|-----|-------------|
| `latest` | Latest stable build from main branch |
| `v1.0.0` | Specific version release |
| `<sha>` | Specific commit SHA |

## Runtime Installation

Language runtimes (Go, Python, Rust, etc.) are **not** included in the base images to keep them minimal. Agents can install required runtimes on-demand within sandboxes using skills.

## Development

### Local Development Workflow

```bash
# Start local Docker registry
make registry-up

# Build all agent images (base + per-agent)
make agents-build

# Build a single agent image
make agents-build-one AGENT=claude

# Smoke test all agents
make agents-test

# Full E2E test with nanosandbox
make agents-e2e

# Stop registry
make registry-down
```

### Image Architecture

Each agent has its own slim Alpine-based image built on a shared base:

| Image | Base | Added CLI | Est. Size |
|-------|------|-----------|-----------|
| `dd-agents-base` | Alpine 3.20 + Node.js 22 + agent-gateway + MCP packages | - | ~150MB |
| `dd-agent-claude` | dd-agents-base | @anthropic-ai/claude-code | ~200MB |
| `dd-agent-goose` | dd-agents-base | goose binary | ~180MB |
| `dd-agent-codex` | dd-agents-base | @openai/codex | ~200MB |
| `dd-agent-cursor` | dd-agents-base | cursor-agent binary | ~180MB |

### Context Sharing Between Agents

Agents share context via:
- **Workspace mount**: `/workspace` is shared across all VMs via virtio-fs
- **MCP Memory server**: Persists knowledge graph to `/workspace/.memory/`, readable by any agent
- **Git history**: All file changes tracked in `/workspace/.git/`

## CI/CD

This repository uses GitHub Actions to automatically build and push Docker images to GitHub Container Registry on:

- Push to `main` branch
- Version tags (`v*`)
- Manual workflow dispatch

Both Linux and Windows images are built in parallel:
- **Linux:** Built on `ubuntu-latest` with multi-platform support (amd64, arm64)
- **Windows:** Built on `windows-2022` for Windows Server 2022 LTSC

## License

MIT
