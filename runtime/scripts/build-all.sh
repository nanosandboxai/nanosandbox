#!/bin/bash
# Build all runtime components from source.
#
# Usage:
#   ./scripts/build-all.sh              # build everything
#   ./scripts/build-all.sh libkrunfw    # build only libkrunfw
#   ./scripts/build-all.sh libkrun      # build only libkrun (upstream)
#   ./scripts/build-all.sh gvproxy      # build only gvproxy
#   ./scripts/build-all.sh runtime      # build only runtime crate
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILD_DIR="$ROOT_DIR/build"

mkdir -p "$BUILD_DIR/lib" "$BUILD_DIR/bin"

COMPONENT="${1:-all}"

build_libkrunfw() {
    echo "==> Building libkrunfw..."
    if [ ! -f "$ROOT_DIR/deps/libkrunfw/Makefile" ]; then
        echo "ERROR: libkrunfw submodule not initialized. Run: git submodule update --init --recursive"
        exit 1
    fi
    cd "$ROOT_DIR/deps/libkrunfw"
    make
    # Copy built library to build/lib/
    find . -name 'libkrunfw*.so*' -o -name 'libkrunfw*.dylib' | head -5 | while read -r f; do
        cp "$f" "$BUILD_DIR/lib/"
    done
    echo "==> libkrunfw built → $BUILD_DIR/lib/"
}

build_libkrun() {
    echo "==> Building libkrun (upstream v1.19.5)..."
    bash "$SCRIPT_DIR/build-libkrun.sh"
    # Copy built library to build/lib/
    LIBKRUN_OUTPUT_DIR="${LIBKRUN_OUTPUT_DIR:-$HOME/.nanosandbox/lib}"
    if [ -f "$LIBKRUN_OUTPUT_DIR/libkrun.a" ]; then
        cp "$LIBKRUN_OUTPUT_DIR/libkrun.a" "$BUILD_DIR/lib/"
        echo "==> libkrun built → $BUILD_DIR/lib/libkrun.a"
    fi
}

build_gvproxy() {
    echo "==> Building gvproxy..."
    if [ ! -f "$ROOT_DIR/deps/gvproxy/go.mod" ]; then
        echo "ERROR: gvproxy submodule not initialized. Run: git submodule update --init --recursive"
        exit 1
    fi
    cd "$ROOT_DIR/deps/gvproxy"
    CGO_ENABLED=0 go build -ldflags="-s -w" -o "$BUILD_DIR/bin/gvproxy" ./cmd/gvproxy
    echo "==> gvproxy built → $BUILD_DIR/bin/gvproxy"
}

build_runtime() {
    echo "==> Building runtime..."
    cd "$ROOT_DIR"
    # Ensure libkrun is built first
    if [ ! -f "${LIBKRUN_LIB_DIR:-$HOME/.nanosandbox/lib}/libkrun.a" ]; then
        echo "WARNING: libkrun.a not found. Run './scripts/build-all.sh libkrun' first."
        echo "         Or set LIBKRUN_LIB_DIR to the directory containing libkrun.a."
    fi
    cargo build --release -p runtime
    echo "==> runtime built"
}

case "$COMPONENT" in
    libkrunfw) build_libkrunfw ;;
    libkrun)   build_libkrun ;;
    gvproxy)   build_gvproxy ;;
    runtime) build_runtime ;;
    all)
        build_libkrunfw
        build_libkrun
        build_gvproxy
        build_runtime
        ;;
    *)
        echo "Unknown component: $COMPONENT"
        echo "Usage: $0 [all|libkrunfw|libkrun|gvproxy|runtime]"
        exit 1
        ;;
esac

echo "==> Build complete. Artifacts in $BUILD_DIR/"
