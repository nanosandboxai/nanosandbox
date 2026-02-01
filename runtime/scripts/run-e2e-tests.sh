#!/bin/bash
#
# Run Nanosandbox tests
#
# Usage:
#   ./scripts/run-e2e-tests.sh [OPTIONS]
#
# Options:
#   --verbose         Show detailed test output
#   --help            Show this help message

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Default options
verbose=false
test_threads=1

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --verbose)
            verbose=true
            shift
            ;;
        --help|-h)
            echo "Nanosandbox Test Runner"
            echo ""
            echo "Usage: ./scripts/run-e2e-tests.sh [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --verbose         Show detailed test output"
            echo "  --help            Show this help message"
            echo ""
            echo "All tests run by default, including E2E tests that require network."
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
done

cd "$PROJECT_DIR"

echo "=========================================="
echo "Nanosandbox Tests"
echo "=========================================="
echo ""

# Check prerequisites first
echo "Checking prerequisites..."
"$SCRIPT_DIR/check-e2e-prereqs.sh" --network-only || true
echo ""

# Build test arguments
CARGO_ARGS=("test" "--features" "cli" "--")

if $verbose; then
    CARGO_ARGS+=("--nocapture")
fi

CARGO_ARGS+=("--test-threads=$test_threads")

echo "Running: cargo ${CARGO_ARGS[*]}"
echo ""

# Run the tests
cargo "${CARGO_ARGS[@]}"

echo ""
echo "=========================================="
echo "Tests Complete"
echo "=========================================="
