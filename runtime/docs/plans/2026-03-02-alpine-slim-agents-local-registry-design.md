# Alpine Slim Agents + Local Registry Design

**Date:** 2026-03-02
**Status:** Approved

## Problem

The dd-agents-registry Dockerfile bundles ALL 5 agents into a single Debian Bookworm Slim image (~1GB+). This is too large for fast iteration during local development with nanosandbox. There is no local registry workflow, and no mechanism for agents to share context when users switch between them.

## Goals

1. Per-agent Alpine-based Docker images (~180-250MB each) with maximum layer sharing
2. Local registry workflow for fast build-push-test cycle
3. MCP Memory-based context sharing between agents via shared workspace
4. Full E2E validation using nanosandbox SDK (pull from local registry, boot VM, test agent-gateway)

## Non-Goals

- Windows container images (deferred, current focus is Linux/macOS)
- Custom context protocol (use existing MCP memory server)
- Distroless images (MCP servers need npm/npx runtime)

## Architecture

### Docker Image Hierarchy

```
┌─────────────────────────────────────────────────────┐
│              dd-agents-base (Alpine 3.20)            │
│  ~150MB shared base layer                            │
│                                                      │
│  ┌──────────────┐ ┌──────────┐ ┌──────────────────┐ │
│  │ agent-gateway │ │ Node.js  │ │ MCP npm packages │ │
│  │ (Go static)   │ │ 22 LTS   │ │ (github, memory, │ │
│  │               │ │ (Alpine) │ │  filesystem, c7)  │ │
│  └──────────────┘ └──────────┘ └──────────────────┘ │
│  + git, curl, ca-certs, iproute2                     │
└──────────────┬───────────────────────────────────────┘
               │ FROM dd-agents-base
    ┌──────────┼──────────┬──────────┬──────────┐
    ▼          ▼          ▼          ▼          ▼
┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐
│ claude │ │opencode│ │ goose  │ │ codex  │ │ cursor │
│~200MB  │ │~200MB  │ │~180MB  │ │~200MB  │ │~180MB  │
│npm pkg │ │npm pkg │ │binary  │ │npm pkg │ │binary  │
└────────┘ └────────┘ └────────┘ └────────┘ └────────┘
```

### Key Decisions

- **Alpine 3.20** base instead of Debian Bookworm Slim (saves ~50MB)
- **Agent-gateway** built as static Go binary via multi-stage build (`CGO_ENABLED=0 GOOS=linux`)
- **Node.js 22 LTS** Alpine variant (not nodesource.com script)
- **MCP memory server** configured to persist to `/workspace/.memory/` for cross-agent context
- Each agent image adds only its CLI (~30-80MB on top of base)

## File Structure

New files added to `dd-nanosandbox`:

```
docker/
  Dockerfile.base          # Shared Alpine base: gateway + Node.js + MCP packages
  Dockerfile.claude        # FROM base, adds @anthropic-ai/claude-code
  Dockerfile.opencode      # FROM base, adds opencode-ai
  Dockerfile.goose         # FROM base, adds goose binary (direct download)
  Dockerfile.codex         # FROM base, adds @openai/codex
  Dockerfile.cursor        # FROM base, adds cursor-agent binary
  docker-compose.yml       # Local registry + build orchestration
  .dockerignore            # Exclude target/, .git, etc.
```

## Local Registry Workflow

### Makefile Targets

```
make registry-up           # Start localhost:5000 registry:2 container
make agents-build-base     # Build + push base image to local registry
make agents-build          # Build + push all 5 agent images
make agents-build AGENT=claude  # Build single agent image
make agents-test           # Smoke test: run --version for each agent
make agents-e2e            # Full nanosandbox E2E test
make registry-down         # Stop local registry
```

### docker-compose.yml

Services:
1. `registry` - `registry:2` on port 5000
2. Base image built and pushed first
3. Per-agent images built FROM base and pushed to local registry

### CI Integration

Update `.github/workflows/docker-build.yml`:
- Build base image first, then agent images in parallel
- Push per-agent images to `ghcr.io/devdone-labs/dd-agent-<name>`
- Keep backward compat tag `dd-agents:latest` pointing to a combo or claude image

## MCP Memory Context Sharing

### Mechanism

All agent VMs mount `/workspace` via virtio-fs. The MCP memory server is configured to store its knowledge graph at `/workspace/.memory/memory.json`. When a user switches agents (destroys one VM, creates another with a different agent image), the new agent's MCP memory server reads the same persistent store.

```
Host: /workspace/.memory/memory.json  ← persists across VMs
  ↕ virtio-fs mount
VM-1 (claude): MCP memory server reads/writes
  ↕ user switches agent
VM-2 (goose): MCP memory server reads same file
```

### mcp-servers.yaml Change

```yaml
memory:
  command: "npx"
  args: ["-y", "@modelcontextprotocol/server-memory", "--directory", "/workspace/.memory"]
  enabled: true
```

### What Gets Shared

- **Knowledge graph** via MCP memory server (entities, relations, observations)
- **File changes** via git history in `/workspace/.git/`
- **Project files** directly in `/workspace/`

### What Does NOT Get Shared

- Agent-specific conversation history (Claude sessions in `~/.claude/`, Goose in `~/.config/goose/`)
- Agent-specific settings and preferences
- This is by design: conversation history is agent-specific and not portable

## E2E Validation

### Test Script: `scripts/test-agents-e2e.sh`

For each agent (claude, opencode, goose, codex, cursor):

1. `nanosb pull localhost:5000/dd-agent-<name>:latest` - Pull from local registry
2. `nanosb run localhost:5000/dd-agent-<name>:latest` - Boot VM
3. Poll `GET /health` until agent-gateway is ready
4. `POST /api/v1/exec {"command":"<agent>","args":["--version"]}` - Verify agent binary works
5. `GET /api/v1/mcp/servers` - Verify MCP servers are configured
6. `POST /api/v1/message {"message":"hello","agent":"<agent>"}` - Test SSE streaming (optional, needs API keys)
7. `nanosb stop <id>` - Cleanup

### Prerequisites

- libkrun installed (macOS Apple Silicon)
- Local registry running (`make registry-up`)
- Agent images built and pushed (`make agents-build`)
- API keys optional (steps 1-5 work without them, step 6 needs keys)

## Estimated Image Sizes

| Image | Contents | Est. Size |
|-------|----------|-----------|
| dd-agents-base | Alpine + Node.js 22 + agent-gateway + MCP packages + git | ~150MB |
| dd-agent-claude | base + @anthropic-ai/claude-code | ~200MB |
| dd-agent-opencode | base + opencode-ai | ~200MB |
| dd-agent-goose | base + goose binary | ~180MB |
| dd-agent-codex | base + @openai/codex | ~200MB |
| dd-agent-cursor | base + cursor-agent binary | ~180MB |

vs. current monolithic image: **~1GB+**
