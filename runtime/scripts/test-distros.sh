#!/bin/bash
# Run multi-distro build tests using Docker.
#
# Usage:
#   ./scripts/test-distros.sh              # test all distros
#   ./scripts/test-distros.sh alpine       # test single distro
#   ./scripts/test-distros.sh ubuntu debian # test multiple distros
#
# Supported distros: ubuntu, debian, fedora, archlinux, alpine, opensuse
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
COMPOSE_FILE="$ROOT_DIR/docker/test-distros/docker-compose.yml"

ALL_DISTROS=(ubuntu debian fedora archlinux alpine opensuse)
if [ $# -gt 0 ]; then
    DISTROS=("$@")
else
    DISTROS=("${ALL_DISTROS[@]}")
fi

PASSED=()
FAILED=()

echo "============================================"
echo " Multi-Distro Build Test"
echo " Distros: ${DISTROS[*]}"
echo "============================================"
echo ""

for distro in "${DISTROS[@]}"; do
    echo ">>> Building and testing on: $distro"
    echo "--------------------------------------------"

    if docker compose -f "$COMPOSE_FILE" build "$distro" 2>&1; then
        if docker compose -f "$COMPOSE_FILE" run --rm "$distro" 2>&1; then
            echo ">>> PASS: $distro"
            PASSED+=("$distro")
        else
            echo ">>> FAIL: $distro (run failed)"
            FAILED+=("$distro")
        fi
    else
        echo ">>> FAIL: $distro (build failed)"
        FAILED+=("$distro")
    fi
    echo ""
done

echo "============================================"
echo " Results"
echo "============================================"
echo " Passed (${#PASSED[@]}): ${PASSED[*]:-none}"
echo " Failed (${#FAILED[@]}): ${FAILED[*]:-none}"
echo "============================================"

if [ ${#FAILED[@]} -gt 0 ]; then
    exit 1
fi
