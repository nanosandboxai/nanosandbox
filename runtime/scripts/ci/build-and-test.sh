#!/bin/bash
# Build and test all C/Rust/Go components (used in Docker multi-distro testing).
#
# This script does NOT require KVM — it only compiles and runs unit tests.
# Integration tests that boot VMs are gated behind the "integration-tests" feature.
# The Go agent-gateway has been removed; guest setup lives at the microVM layer.
set -euo pipefail

source "$HOME/.cargo/env" 2>/dev/null || true

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

echo "==> [1/6] Building libkrunfw..."
cd "$ROOT_DIR/deps/libkrunfw"
make -j"$(nproc)"

echo "==> [2/6] Building upstream libkrun..."
bash "$SCRIPT_DIR/../build-libkrun.sh"

echo "==> [3/6] Building gvproxy..."
cd "$ROOT_DIR/deps/gvproxy"
if command -v go &>/dev/null; then
    CGO_ENABLED=0 go build -ldflags="-s -w" -o gvproxy ./cmd/gvproxy
    echo "gvproxy built: $(ls -la gvproxy)"
else
    echo "Go not available — skipping gvproxy build"
fi

echo "==> [4/6] Building Rust crates..."
cd "$ROOT_DIR"
cargo build --release -p libkrun-sys -p runtime

echo "==> [5/6] Running unit tests..."
cargo test -p libkrun-sys -p runtime 2>&1

echo "==> [6/6] Verifying artifacts..."
echo "--- libkrunfw ---"
find "$ROOT_DIR/deps/libkrunfw" -name 'libkrunfw*.so*' | head -5
echo "--- libkrun ---"
ls -la "$HOME/.nanosandbox/lib/libkrun.a" 2>/dev/null || echo "(not found)"
echo "--- gvproxy ---"
ls -la "$ROOT_DIR/deps/gvproxy/gvproxy" 2>/dev/null || echo "(not built)"

echo ""
echo "==> All builds and unit tests passed."
echo "NOTE: Integration tests requiring KVM are gated behind the 'integration-tests' feature."
