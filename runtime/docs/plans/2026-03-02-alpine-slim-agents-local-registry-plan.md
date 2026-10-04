# Alpine Slim Agents + Local Registry Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the monolithic 1GB+ Debian agent image with per-agent Alpine images (~180-250MB each), add a local registry workflow for fast dev iteration, enable MCP memory-based context sharing between agents, and validate the full pipeline with nanosandbox E2E tests.

**Architecture:** Shared Alpine base image (agent-gateway + Node.js + MCP packages) with per-agent Dockerfiles that add only their CLI. Local `registry:2` on localhost:5000 for build-push-test cycle. MCP memory server persists to `/workspace/.memory/` for cross-agent context. Makefile targets orchestrate the entire workflow.

**Tech Stack:** Docker (multi-stage builds, Alpine 3.20), Go 1.22 (agent-gateway), Node.js 22 LTS, Docker Compose, Make, Bash, nanosandbox CLI

---

## Task 1: Create .dockerignore

**Files:**
- Create: `docker/.dockerignore`

**Step 1: Write the .dockerignore file**

```dockerignore
# Rust build artifacts
target/

# Git
.git/
.gitignore

# IDE
.idea/
.vscode/
*.swp

# OS
.DS_Store
Thumbs.db

# Docs and tests (not needed in images)
docs/
tests/
*.md
!README.Agents.md

# Environment
.env

# Logs
*.log
```

**Step 2: Verify file exists**

Run: `cat docker/.dockerignore`
Expected: File contents shown above

**Step 3: Commit**

```bash
git add docker/.dockerignore
git commit -m "chore: add docker/.dockerignore for slim builds"
```

---

## Task 2: Create Dockerfile.base (Shared Alpine Base Image)

**Files:**
- Create: `docker/Dockerfile.base`

This is the most critical file. It builds the shared base layer used by all per-agent images.

**Step 1: Write Dockerfile.base**

```dockerfile
# =============================================================================
# Stage 1: Build agent-gateway as a static Go binary
# =============================================================================
FROM golang:1.22-alpine AS gateway-builder
WORKDIR /build
COPY agent-gateway/ .
RUN CGO_ENABLED=0 GOOS=linux go build -ldflags="-s -w" -o agent-gateway .

# =============================================================================
# Stage 2: Alpine base with Node.js, MCP packages, and agent-gateway
# =============================================================================
FROM node:22-alpine

LABEL org.opencontainers.image.source="https://github.com/devdone-labs/dd-nanosandbox"
LABEL org.opencontainers.image.description="Shared base image for DD agent containers"
LABEL org.opencontainers.image.licenses="MIT"

# System dependencies (minimal set for agents + networking)
RUN apk add --no-cache \
    curl \
    ca-certificates \
    git \
    iproute2 \
    bash

# Copy agent-gateway binary from builder
COPY --from=gateway-builder /build/agent-gateway /usr/local/bin/agent-gateway

# Pre-install MCP server npm packages globally (avoids npx download at runtime)
RUN npm install -g \
    @modelcontextprotocol/server-github \
    @modelcontextprotocol/server-filesystem \
    @modelcontextprotocol/server-memory \
    @modelcontextprotocol/server-brave-search \
    @upstash/context7-mcp \
    && npm cache clean --force

# Create config directories for MCP config generation
RUN mkdir -p /root/.config/goose \
    && mkdir -p /root/.codex \
    && mkdir -p /root/.config/opencode \
    && mkdir -p /workspace/.cursor \
    && mkdir -p /workspace/.memory

WORKDIR /workspace

# Verify gateway binary works
RUN test -x /usr/local/bin/agent-gateway && echo "agent-gateway: OK"
```

**Step 2: Test build locally**

Run: `docker build -f docker/Dockerfile.base -t dd-agents-base:latest .`
Expected: Build succeeds, output includes "agent-gateway: OK"

**Step 3: Check image size**

Run: `docker images dd-agents-base:latest --format "{{.Size}}"`
Expected: ~150-200MB

**Step 4: Commit**

```bash
git add docker/Dockerfile.base
git commit -m "feat: add Alpine base Dockerfile with agent-gateway and MCP packages"
```

---

## Task 3: Create Per-Agent Dockerfiles

**Files:**
- Create: `docker/Dockerfile.claude`
- Create: `docker/Dockerfile.opencode`
- Create: `docker/Dockerfile.goose`
- Create: `docker/Dockerfile.codex`
- Create: `docker/Dockerfile.cursor`

Each agent Dockerfile follows the same pattern: FROM base, install one agent, verify.

**Step 1: Write Dockerfile.claude**

```dockerfile
ARG BASE_IMAGE=dd-agents-base:latest
FROM ${BASE_IMAGE}

LABEL org.opencontainers.image.description="Claude Code agent for DD sandboxes"

RUN npm install -g @anthropic-ai/claude-code \
    && npm cache clean --force

RUN claude --version && echo "Claude Code: OK"

CMD ["agent-gateway"]
```

**Step 2: Write Dockerfile.opencode**

```dockerfile
ARG BASE_IMAGE=dd-agents-base:latest
FROM ${BASE_IMAGE}

LABEL org.opencontainers.image.description="OpenCode agent for DD sandboxes"

RUN npm install -g opencode-ai \
    && npm cache clean --force

RUN opencode --version && echo "OpenCode: OK"

CMD ["agent-gateway"]
```

**Step 3: Write Dockerfile.goose**

```dockerfile
ARG BASE_IMAGE=dd-agents-base:latest
FROM ${BASE_IMAGE}

LABEL org.opencontainers.image.description="Goose agent for DD sandboxes"

ARG TARGETARCH

RUN set -eux; \
    case "${TARGETARCH}" in \
        amd64) GOOSE_ARCH="x86_64-unknown-linux-musl" ;; \
        arm64) GOOSE_ARCH="aarch64-unknown-linux-musl" ;; \
        *) echo "Unsupported architecture: ${TARGETARCH}" && exit 1 ;; \
    esac; \
    curl -fsSL -o /tmp/goose.tar.bz2 \
        "https://github.com/block/goose/releases/latest/download/goose-${GOOSE_ARCH}.tar.bz2" \
    && tar -xjf /tmp/goose.tar.bz2 -C /tmp \
    && mv /tmp/goose /usr/local/bin/goose \
    && chmod +x /usr/local/bin/goose \
    && rm -rf /tmp/goose.tar.bz2

# Install bzip2 for extraction (remove after)
RUN apk add --no-cache bzip2

RUN goose --version && echo "Goose: OK"

CMD ["agent-gateway"]
```

> **Note:** Goose publishes musl builds for Alpine. If musl builds are not available, fall back to `gnu` suffix and install `libc6-compat` via apk. The engineer should check https://github.com/block/goose/releases for exact asset names and adjust `GOOSE_ARCH` accordingly.

**Step 4: Write Dockerfile.codex**

```dockerfile
ARG BASE_IMAGE=dd-agents-base:latest
FROM ${BASE_IMAGE}

LABEL org.opencontainers.image.description="Codex agent for DD sandboxes"

RUN npm install -g @openai/codex \
    && npm cache clean --force

RUN codex --version && echo "Codex: OK"

CMD ["agent-gateway"]
```

**Step 5: Write Dockerfile.cursor**

```dockerfile
ARG BASE_IMAGE=dd-agents-base:latest
FROM ${BASE_IMAGE}

LABEL org.opencontainers.image.description="Cursor agent for DD sandboxes"

RUN curl -fsSL https://cursor.com/install | bash \
    && mv /root/.local/bin/agent /usr/local/bin/cursor-agent \
    && chmod +x /usr/local/bin/cursor-agent

RUN cursor-agent --version && echo "Cursor CLI: OK"

CMD ["agent-gateway"]
```

> **Note:** Cursor's install script may not work on Alpine. If it fails, the engineer should check if Cursor provides a static binary download URL and use `curl` directly. If no Alpine-compatible binary exists, create a stub script that prints "Cursor CLI not available on Alpine" and document the limitation.

**Step 6: Test build one agent (claude) against local base**

Run: `docker build -f docker/Dockerfile.claude --build-arg BASE_IMAGE=dd-agents-base:latest -t dd-agent-claude:latest .`
Expected: Build succeeds, output includes "Claude Code: OK"

**Step 7: Commit**

```bash
git add docker/Dockerfile.claude docker/Dockerfile.opencode docker/Dockerfile.goose docker/Dockerfile.codex docker/Dockerfile.cursor
git commit -m "feat: add per-agent Alpine Dockerfiles for all 5 agents"
```

---

## Task 4: Create docker-compose.yml for Local Registry

**Files:**
- Create: `docker/docker-compose.yml`

**Step 1: Write docker-compose.yml**

```yaml
version: "3.8"

services:
  registry:
    image: registry:2
    ports:
      - "5000:5000"
    restart: unless-stopped
    volumes:
      - registry-data:/var/lib/registry

volumes:
  registry-data:
```

> **Why minimal:** The docker-compose only manages the registry. Image builds are driven by Makefile targets which give more control over build order (base first, then agents).

**Step 2: Verify registry starts**

Run: `docker compose -f docker/docker-compose.yml up -d`
Expected: Registry container starts on port 5000

Run: `curl -s http://localhost:5000/v2/_catalog`
Expected: `{"repositories":[]}`

**Step 3: Stop registry**

Run: `docker compose -f docker/docker-compose.yml down`

**Step 4: Commit**

```bash
git add docker/docker-compose.yml
git commit -m "feat: add docker-compose.yml with local registry:2"
```

---

## Task 5: Update mcp-servers.yaml for Shared Memory Persistence

**Files:**
- Modify: `agent-gateway/mcp-servers.yaml:25-27`

**Step 1: Update memory server args to persist to /workspace/.memory**

Change the `memory` server entry from:

```yaml
  memory:
    command: "npx"
    args: ["-y", "@modelcontextprotocol/server-memory"]
    enabled: true
```

To:

```yaml
  memory:
    command: "npx"
    args: ["-y", "@modelcontextprotocol/server-memory", "--directory", "/workspace/.memory"]
    enabled: true
```

**Step 2: Verify YAML is valid**

Run: `cd agent-gateway && go test ./mcp/ -run TestParseConfig -v`
Expected: Tests pass (the existing tests parse mcp-servers.yaml)

> **Note:** If no test named `TestParseConfig` exists, run `go test ./mcp/ -v` to run all MCP tests. The manager tests in `agent-gateway/mcp/manager_test.go` parse YAML configs and will catch syntax errors.

**Step 3: Commit**

```bash
git add agent-gateway/mcp-servers.yaml
git commit -m "feat: configure MCP memory server to persist to /workspace/.memory"
```

---

## Task 6: Add Agent Docker Targets to Makefile

**Files:**
- Modify: `Makefile` (append new section after line 109)

**Step 1: Add the agent Docker section to Makefile**

Append after the existing `ci-fast` target:

```makefile

# =============================================================================
# Agent Docker Image Targets
# =============================================================================

REGISTRY_HOST ?= localhost:5000
BASE_IMAGE_TAG ?= latest
AGENTS := claude opencode goose codex cursor

.PHONY: registry-up registry-down agents-build-base agents-build agents-test agents-e2e

# Start local Docker registry
registry-up:
	@docker compose -f docker/docker-compose.yml up -d
	@echo "Registry running at $(REGISTRY_HOST)"
	@echo "Waiting for registry..."
	@until curl -sf http://$(REGISTRY_HOST)/v2/_catalog > /dev/null 2>&1; do sleep 1; done
	@echo "Registry ready"

# Stop local Docker registry
registry-down:
	@docker compose -f docker/docker-compose.yml down

# Build and push base image to local registry
agents-build-base:
	docker build -f docker/Dockerfile.base -t $(REGISTRY_HOST)/dd-agents-base:$(BASE_IMAGE_TAG) .
	docker push $(REGISTRY_HOST)/dd-agents-base:$(BASE_IMAGE_TAG)
	@echo "Base image pushed to $(REGISTRY_HOST)/dd-agents-base:$(BASE_IMAGE_TAG)"

# Build and push a single agent (usage: make agents-build AGENT=claude)
agents-build-one:
ifndef AGENT
	$(error AGENT is required. Usage: make agents-build-one AGENT=claude)
endif
	docker build -f docker/Dockerfile.$(AGENT) \
		--build-arg BASE_IMAGE=$(REGISTRY_HOST)/dd-agents-base:$(BASE_IMAGE_TAG) \
		-t $(REGISTRY_HOST)/dd-agent-$(AGENT):$(BASE_IMAGE_TAG) .
	docker push $(REGISTRY_HOST)/dd-agent-$(AGENT):$(BASE_IMAGE_TAG)
	@echo "$(AGENT) image pushed to $(REGISTRY_HOST)/dd-agent-$(AGENT):$(BASE_IMAGE_TAG)"

# Build and push all agent images (builds base first)
agents-build: agents-build-base
	@for agent in $(AGENTS); do \
		echo "Building $$agent..."; \
		docker build -f docker/Dockerfile.$$agent \
			--build-arg BASE_IMAGE=$(REGISTRY_HOST)/dd-agents-base:$(BASE_IMAGE_TAG) \
			-t $(REGISTRY_HOST)/dd-agent-$$agent:$(BASE_IMAGE_TAG) . \
		&& docker push $(REGISTRY_HOST)/dd-agent-$$agent:$(BASE_IMAGE_TAG) \
		|| exit 1; \
	done
	@echo "All agent images built and pushed"

# Smoke test: verify each agent image runs --version
agents-test:
	@for agent in $(AGENTS); do \
		echo "Testing $$agent..."; \
		docker run --rm $(REGISTRY_HOST)/dd-agent-$$agent:$(BASE_IMAGE_TAG) $$agent --version \
		&& echo "  $$agent: OK" \
		|| echo "  $$agent: FAILED"; \
	done

# Show local registry contents
agents-list:
	@curl -sf http://$(REGISTRY_HOST)/v2/_catalog | python3 -m json.tool 2>/dev/null \
		|| curl -sf http://$(REGISTRY_HOST)/v2/_catalog

# Full E2E test with nanosandbox
agents-e2e:
	@./scripts/test-agents-e2e.sh
```

**Step 2: Update .PHONY and help at top of Makefile**

Update the `.PHONY` line (line 5-6) to include new targets:

```makefile
.PHONY: help build build-release test test-unit test-e2e test-e2e-network \
        install-runtime check-prereqs clean clippy fmt lint \
        registry-up registry-down agents-build-base agents-build agents-build-one \
        agents-test agents-list agents-e2e
```

Add to the `help` target output (after the "Development targets" echo block, before the empty line):

```makefile
	@echo ""
	@echo "Agent Docker targets:"
	@echo "  registry-up       Start local Docker registry on localhost:5000"
	@echo "  registry-down     Stop local Docker registry"
	@echo "  agents-build-base Build and push shared base image"
	@echo "  agents-build      Build and push all agent images (includes base)"
	@echo "  agents-build-one  Build single agent (AGENT=claude|opencode|goose|codex|cursor)"
	@echo "  agents-test       Smoke test all agent images (--version)"
	@echo "  agents-list       Show images in local registry"
	@echo "  agents-e2e        Run full E2E tests with nanosandbox"
```

**Step 3: Verify Makefile syntax**

Run: `make help`
Expected: New "Agent Docker targets" section appears in help output

**Step 4: Commit**

```bash
git add Makefile
git commit -m "feat: add Makefile targets for agent Docker builds and local registry"
```

---

## Task 7: Create E2E Test Script

**Files:**
- Create: `scripts/test-agents-e2e.sh`

**Step 1: Write the E2E test script**

```bash
#!/usr/bin/env bash
# test-agents-e2e.sh - Full E2E test of agent images with nanosandbox
#
# Prerequisites:
#   - libkrun installed (macOS Apple Silicon)
#   - Local registry running (make registry-up)
#   - Agent images built (make agents-build)
#
# Usage:
#   ./scripts/test-agents-e2e.sh              # Test all agents
#   ./scripts/test-agents-e2e.sh claude goose # Test specific agents

set -euo pipefail

REGISTRY_HOST="${REGISTRY_HOST:-localhost:5000}"
TAG="${TAG:-latest}"
NANOSB="${NANOSB:-./target/release/nanosb}"
HEALTH_TIMEOUT=30  # seconds to wait for agent-gateway health
PASSED=0
FAILED=0
SKIPPED=0

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

# Agents to test: use args or default to all
if [ $# -gt 0 ]; then
    AGENTS=("$@")
else
    AGENTS=(claude opencode goose codex cursor)
fi

echo "=== Agent E2E Tests ==="
echo "Registry: ${REGISTRY_HOST}"
echo "Agents:   ${AGENTS[*]}"
echo ""

# Check prerequisites
if [ ! -x "$NANOSB" ]; then
    echo -e "${RED}ERROR: nanosb binary not found at ${NANOSB}${NC}"
    echo "Run: cargo build --release --features cli"
    exit 1
fi

for AGENT in "${AGENTS[@]}"; do
    IMAGE="${REGISTRY_HOST}/dd-agent-${AGENT}:${TAG}"
    echo "--- Testing ${AGENT} (${IMAGE}) ---"

    # Step 1: Pull image from local registry
    echo "  [1/5] Pulling image..."
    if ! $NANOSB pull "${IMAGE}" 2>&1; then
        echo -e "  ${RED}FAIL: Could not pull ${IMAGE}${NC}"
        ((FAILED++))
        continue
    fi

    # Step 2: Start sandbox
    echo "  [2/5] Starting sandbox..."
    SANDBOX_ID=$($NANOSB run -d "${IMAGE}" 2>&1 | grep -oE '[a-f0-9-]{36}' | head -1)
    if [ -z "$SANDBOX_ID" ]; then
        echo -e "  ${RED}FAIL: Could not start sandbox for ${AGENT}${NC}"
        ((FAILED++))
        continue
    fi
    echo "  Sandbox ID: ${SANDBOX_ID}"

    # Step 3: Wait for agent-gateway health
    echo "  [3/5] Waiting for agent-gateway..."
    HEALTHY=false
    for i in $(seq 1 $HEALTH_TIMEOUT); do
        if curl -sf "http://localhost:8080/health" > /dev/null 2>&1; then
            HEALTHY=true
            break
        fi
        sleep 1
    done

    if [ "$HEALTHY" != "true" ]; then
        echo -e "  ${YELLOW}SKIP: agent-gateway did not become healthy in ${HEALTH_TIMEOUT}s${NC}"
        $NANOSB stop "$SANDBOX_ID" 2>/dev/null || true
        ((SKIPPED++))
        continue
    fi
    echo "  agent-gateway is healthy"

    # Step 4: Test agent --version via exec endpoint
    echo "  [4/5] Testing ${AGENT} --version..."
    EXEC_RESULT=$(curl -sf -X POST http://localhost:8080/api/v1/exec \
        -H "Content-Type: application/json" \
        -d "{\"command\":\"${AGENT}\",\"args\":[\"--version\"],\"timeout\":10}" 2>&1 || true)

    if echo "$EXEC_RESULT" | grep -q '"type":"exit"'; then
        echo -e "  ${GREEN}${AGENT} --version: OK${NC}"
    else
        echo -e "  ${YELLOW}${AGENT} --version: could not verify (may need API key)${NC}"
    fi

    # Step 5: Test MCP servers endpoint
    echo "  [5/5] Testing MCP servers..."
    MCP_RESULT=$(curl -sf http://localhost:8080/api/v1/mcp/servers 2>&1 || true)
    if echo "$MCP_RESULT" | grep -q '"memory"'; then
        echo -e "  ${GREEN}MCP servers: OK (memory server present)${NC}"
    else
        echo -e "  ${YELLOW}MCP servers: could not verify${NC}"
    fi

    # Cleanup
    echo "  Stopping sandbox..."
    $NANOSB stop "$SANDBOX_ID" 2>/dev/null || true

    ((PASSED++))
    echo -e "  ${GREEN}${AGENT}: PASSED${NC}"
    echo ""
done

echo "=== Results ==="
echo -e "  ${GREEN}Passed:  ${PASSED}${NC}"
echo -e "  ${RED}Failed:  ${FAILED}${NC}"
echo -e "  ${YELLOW}Skipped: ${SKIPPED}${NC}"

if [ "$FAILED" -gt 0 ]; then
    exit 1
fi
```

**Step 2: Make script executable**

Run: `chmod +x scripts/test-agents-e2e.sh`

**Step 3: Verify script syntax**

Run: `bash -n scripts/test-agents-e2e.sh`
Expected: No output (valid syntax)

**Step 4: Commit**

```bash
git add scripts/test-agents-e2e.sh
git commit -m "feat: add E2E test script for agent images with nanosandbox"
```

---

## Task 8: Update CI Workflow for Per-Agent Builds

**Files:**
- Modify: `.github/workflows/docker-build.yml`

**Step 1: Rewrite the workflow to build base + per-agent images**

Replace the entire file with:

```yaml
name: Build and Push Agent Docker Images

on:
  push:
    branches:
      - main
      - feature/**
    tags:
      - 'v*'
  pull_request:
    branches:
      - main
  workflow_dispatch:
    inputs:
      tag:
        description: 'Image tag (optional, defaults to SHA)'
        required: false
        type: string
      agent:
        description: 'Specific agent to build (blank = all)'
        required: false
        type: choice
        options:
          - ''
          - claude
          - opencode
          - goose
          - codex
          - cursor

env:
  REGISTRY: ghcr.io
  BASE_IMAGE_NAME: ${{ github.repository_owner }}/dd-agents-base

jobs:
  # ===========================================================================
  # Build shared base image first
  # ===========================================================================
  build-base:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      packages: write
    outputs:
      base-tag: ${{ steps.meta.outputs.version }}
    steps:
      - name: Checkout repository
        uses: actions/checkout@v4

      - name: Set up Docker Buildx
        uses: docker/setup-buildx-action@v3

      - name: Log in to Container Registry
        if: github.event_name != 'pull_request'
        uses: docker/login-action@v3
        with:
          registry: ${{ env.REGISTRY }}
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}

      - name: Extract metadata
        id: meta
        uses: docker/metadata-action@v5
        with:
          images: ${{ env.REGISTRY }}/${{ env.BASE_IMAGE_NAME }}
          tags: |
            type=raw,value=latest,enable=${{ github.ref == 'refs/heads/main' }}
            type=sha,prefix=

      - name: Build and push base image
        uses: docker/build-push-action@v5
        with:
          context: .
          file: ./docker/Dockerfile.base
          push: ${{ github.event_name != 'pull_request' }}
          tags: ${{ steps.meta.outputs.tags }}
          labels: ${{ steps.meta.outputs.labels }}
          cache-from: type=gha,scope=base
          cache-to: type=gha,scope=base,mode=max
          platforms: linux/amd64,linux/arm64

  # ===========================================================================
  # Build per-agent images in parallel (depend on base)
  # ===========================================================================
  build-agent:
    needs: build-base
    runs-on: ubuntu-latest
    permissions:
      contents: read
      packages: write
    strategy:
      matrix:
        agent: [claude, opencode, goose, codex, cursor]
      fail-fast: false
    if: inputs.agent == '' || inputs.agent == matrix.agent

    steps:
      - name: Checkout repository
        uses: actions/checkout@v4

      - name: Set up Docker Buildx
        uses: docker/setup-buildx-action@v3

      - name: Log in to Container Registry
        if: github.event_name != 'pull_request'
        uses: docker/login-action@v3
        with:
          registry: ${{ env.REGISTRY }}
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}

      - name: Extract metadata
        id: meta
        uses: docker/metadata-action@v5
        with:
          images: ${{ env.REGISTRY }}/${{ github.repository_owner }}/dd-agent-${{ matrix.agent }}
          tags: |
            type=raw,value=latest,enable=${{ github.ref == 'refs/heads/main' }}
            type=semver,pattern={{version}}
            type=sha,prefix=
            type=raw,value=${{ inputs.tag }},enable=${{ inputs.tag != '' }}

      - name: Build and push agent image
        uses: docker/build-push-action@v5
        with:
          context: .
          file: ./docker/Dockerfile.${{ matrix.agent }}
          push: ${{ github.event_name != 'pull_request' }}
          tags: ${{ steps.meta.outputs.tags }}
          labels: ${{ steps.meta.outputs.labels }}
          build-args: |
            BASE_IMAGE=${{ env.REGISTRY }}/${{ env.BASE_IMAGE_NAME }}:latest
          cache-from: type=gha,scope=${{ matrix.agent }}
          cache-to: type=gha,scope=${{ matrix.agent }},mode=max
          platforms: linux/amd64,linux/arm64

      - name: Generate build summary
        run: |
          echo "## ${{ matrix.agent }} Agent Image" >> $GITHUB_STEP_SUMMARY
          echo "**Image:** \`${{ github.repository_owner }}/dd-agent-${{ matrix.agent }}\`" >> $GITHUB_STEP_SUMMARY
          echo "### Tags" >> $GITHUB_STEP_SUMMARY
          echo "\`\`\`" >> $GITHUB_STEP_SUMMARY
          echo "${{ steps.meta.outputs.tags }}" >> $GITHUB_STEP_SUMMARY
          echo "\`\`\`" >> $GITHUB_STEP_SUMMARY
```

**Step 2: Verify YAML syntax**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/docker-build.yml'))"`
Expected: No errors

> **Note:** If python3 yaml module is not available, use: `docker run --rm -v $(pwd):/data alpine sh -c 'apk add --no-cache yq && yq . /data/.github/workflows/docker-build.yml > /dev/null'`

**Step 3: Commit**

```bash
git add .github/workflows/docker-build.yml
git commit -m "feat: update CI to build base + per-agent images in parallel"
```

---

## Task 9: Move Dockerfiles from dd-agents-registry (Cleanup)

**Files:**
- Move: `/Users/janvaca/devdone-labs/dd-agents-registry/Dockerfile` -> `docker/Dockerfile.legacy` (for reference)
- Move: `/Users/janvaca/devdone-labs/dd-agents-registry/Dockerfile.windows` -> `docker/Dockerfile.windows` (keep for future)

**Step 1: Copy legacy Dockerfiles into the repo for reference**

```bash
cp /Users/janvaca/devdone-labs/dd-agents-registry/Dockerfile docker/Dockerfile.legacy
cp /Users/janvaca/devdone-labs/dd-agents-registry/Dockerfile.windows docker/Dockerfile.windows
```

**Step 2: Commit**

```bash
git add docker/Dockerfile.legacy docker/Dockerfile.windows
git commit -m "chore: copy legacy Dockerfiles from dd-agents-registry for reference"
```

---

## Task 10: Integration Test - Full Local Build Cycle

This is a manual verification task, not a code task.

**Step 1: Start registry**

Run: `make registry-up`
Expected: "Registry ready" message

**Step 2: Build all images**

Run: `make agents-build`
Expected: Base builds first, then all 5 agents build and push to localhost:5000

**Step 3: Verify registry contents**

Run: `make agents-list`
Expected: Shows dd-agents-base, dd-agent-claude, dd-agent-opencode, dd-agent-goose, dd-agent-codex, dd-agent-cursor

**Step 4: Smoke test**

Run: `make agents-test`
Expected: Each agent prints its version

**Step 5: Check image sizes**

Run: `docker images "localhost:5000/dd-agent*" --format "table {{.Repository}}\t{{.Tag}}\t{{.Size}}"`
Expected: Each image < 300MB

**Step 6: Cleanup**

Run: `make registry-down`

**Step 7: Commit any fixes discovered during testing**

---

## Task 11: E2E Test with Nanosandbox

**Prerequisites:** libkrun installed, nanosb binary built

**Step 1: Build nanosb CLI**

Run: `cargo build --release --features cli`
Expected: Binary at `./target/release/nanosb`

**Step 2: Start registry and build images**

Run: `make registry-up && make agents-build`

**Step 3: Configure nanosandbox for insecure local registry**

The nanosandbox SDK needs to know localhost:5000 is an insecure registry (HTTP, not HTTPS). Check `tests/integration_test.rs:647` for the pattern:

```rust
let config = RegistryConfig::new("localhost:5000").insecure().skip_tls();
```

The E2E test script should set `NANOSB_INSECURE_REGISTRIES=localhost:5000` or the equivalent env var if supported. If not, the engineer may need to add `--insecure-registry localhost:5000` support to the nanosb CLI.

**Step 4: Run E2E tests**

Run: `make agents-e2e`
Expected: Script tests each agent, output shows PASSED/FAILED/SKIPPED

**Step 5: Test single agent manually**

Run: `./scripts/test-agents-e2e.sh claude`
Expected: Claude agent test passes

**Step 6: Commit any fixes**

```bash
git add -A
git commit -m "fix: adjustments from E2E testing"
```

---

## Task 12: Update README.Agents.md

**Files:**
- Modify: `README.Agents.md`

**Step 1: Update the README to reflect the new per-agent architecture**

Update the "Build Locally" section to reference the new workflow:

```markdown
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
| `dd-agent-opencode` | dd-agents-base | opencode-ai | ~200MB |
| `dd-agent-goose` | dd-agents-base | goose binary | ~180MB |
| `dd-agent-codex` | dd-agents-base | @openai/codex | ~200MB |
| `dd-agent-cursor` | dd-agents-base | cursor-agent binary | ~180MB |

### Context Sharing Between Agents

Agents share context via:
- **Workspace mount**: `/workspace` is shared across all VMs via virtio-fs
- **MCP Memory server**: Persists knowledge graph to `/workspace/.memory/`, readable by any agent
- **Git history**: All file changes tracked in `/workspace/.git/`
```

**Step 2: Commit**

```bash
git add README.Agents.md
git commit -m "docs: update README.Agents.md for per-agent Alpine images and local registry"
```

---

## Summary

| Task | Description | Creates/Modifies |
|------|-------------|-----------------|
| 1 | .dockerignore | `docker/.dockerignore` |
| 2 | Base Dockerfile | `docker/Dockerfile.base` |
| 3 | Per-agent Dockerfiles | `docker/Dockerfile.{claude,opencode,goose,codex,cursor}` |
| 4 | docker-compose.yml | `docker/docker-compose.yml` |
| 5 | MCP memory persistence | `agent-gateway/mcp-servers.yaml` |
| 6 | Makefile targets | `Makefile` |
| 7 | E2E test script | `scripts/test-agents-e2e.sh` |
| 8 | CI workflow update | `.github/workflows/docker-build.yml` |
| 9 | Legacy Dockerfile migration | `docker/Dockerfile.{legacy,windows}` |
| 10 | Integration test (manual) | - |
| 11 | Nanosandbox E2E test | - |
| 12 | README update | `README.Agents.md` |
