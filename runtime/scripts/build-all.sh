#!/bin/bash
# Build all runtime components from source.
#
# Usage:
#   ./scripts/build-all.sh              # build everything
#   ./scripts/build-all.sh libkrunfw    # build only libkrunfw
#   ./scripts/build-all.sh libkrun      # build only libkrun
#   ./scripts/build-all.sh gvproxy      # build only gvproxy
#   ./scripts/build-all.sh nanosandbox  # build only nanosandbox
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
    echo "==> Building libkrun..."
    if [ ! -f "$ROOT_DIR/deps/libkrun/Cargo.toml" ]; then
        echo "ERROR: libkrun submodule not initialized. Run: git submodule update --init --recursive"
        exit 1
    fi
    cd "$ROOT_DIR/deps/libkrun"
    PKG_CONFIG_PATH="$BUILD_DIR/lib/pkgconfig:${PKG_CONFIG_PATH:-}" \
        make NET=1 BLK=1
    # Copy built library to build/lib/
    find target/release -name 'libkrun*.so*' -o -name 'libkrun*.dylib' | head -5 | while read -r f; do
        cp "$f" "$BUILD_DIR/lib/"
    done
    echo "==> libkrun built → $BUILD_DIR/lib/"
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

build_nanosandbox() {
    echo "==> Building nanosandbox..."
    cd "$ROOT_DIR"
    cargo build --release -p nanosandbox
    echo "==> nanosandbox built"
}

case "$COMPONENT" in
    libkrunfw) build_libkrunfw ;;
    libkrun)   build_libkrun ;;
    gvproxy)   build_gvproxy ;;
    nanosandbox) build_nanosandbox ;;
    all)
        build_libkrunfw
        build_libkrun
        build_gvproxy
        build_nanosandbox
        ;;
    *)
        echo "Unknown component: $COMPONENT"
        echo "Usage: $0 [all|libkrunfw|libkrun|gvproxy|nanosandbox]"
        exit 1
        ;;
esac

echo "==> Build complete. Artifacts in $BUILD_DIR/"
