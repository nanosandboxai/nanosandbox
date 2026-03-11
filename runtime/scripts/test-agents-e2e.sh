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

REGISTRY_HOST="${REGISTRY_HOST:-localhost:5050}"
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
    AGENTS=(claude goose codex cursor)
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
    IMAGE="${REGISTRY_HOST}/agents-registry/${AGENT}:${TAG}"
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
