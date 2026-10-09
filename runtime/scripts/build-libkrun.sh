#!/usr/bin/env bash
# Build upstream libkrun v1.19.5 as a static library for consumption by nanosandbox.
#
# This script:
#   1. Clones upstream libkrun at a pinned tag (v1.19.5)
#   2. Builds it as a staticlib with net+blk features
#   3. Copies the resulting libkrun.a to an overridable output directory
#
# Usage:
#   ./scripts/build-libkrun.sh                          # default: ~/.nanosandbox/lib/libkrun.a
#   LIBKRUN_OUTPUT_DIR=/path/to/lib ./scripts/build-libkrun.sh
#
# The output directory is created if it doesn't exist.
#
# On macOS, this requires:
#   - brew install lld llvm  (for CC_LINUX cross-compiler and libclang.dylib for bindgen)
#   - LIBCLANG_PATH=$(brew --prefix llvm)/lib
#
# On Linux, the host toolchain is used directly.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ─── Configuration ────────────────────────────────────────────────────────────

# Pinned nanosandbox libkrun revision (fork = source of truth).
# The fork's `nanosandbox` branch carries the customizations as commits:
#   - next-mode init (extra virtiofs mounts + static network)
#   - macOS virtiofs raw-FUSE traversal hardening
# Base upstream tag: v1.19.5.
LIBKRUN_TAG="nanosandbox"
LIBKRUN_SHA="a148cff426923e5a646392a00b2163d80560c964"

# Nanosandbox libkrun fork (source of truth)
LIBKRUN_REPO="https://github.com/nanosandboxai/libkrun.git"

# Build features (net + blk are the ones nanosandbox uses)
LIBKRUN_FEATURES="net,blk"

# Output directory (overridable)
LIBKRUN_OUTPUT_DIR="${LIBKRUN_OUTPUT_DIR:-$HOME/.nanosandbox/lib}"

# Cache directory for the upstream checkout (overridable)
LIBKRUN_CACHE_DIR="${LIBKRUN_CACHE_DIR:-$HOME/.cache/nanosandbox/libkrun}"

# Number of parallel jobs
JOBS="${JOBS:-$(nproc 2>/dev/null || echo 4)}"

# ─── Functions ────────────────────────────────────────────────────────────────

info()  { printf "\033[1;34m==>\033[0m %s\n" "$*"; }
ok()    { printf "\033[1;32m  OK\033[0m %s\n" "$*"; }
err()   { printf "\033[1;31mERROR\033[0m %s\n" "$*" >&2; }

# ─── Main ─────────────────────────────────────────────────────────────────────

info "Building upstream libkrun ${LIBKRUN_TAG}..."

# 1. Clone or update the nanosandbox libkrun fork
if [ -d "$LIBKRUN_CACHE_DIR" ]; then
    info "Updating existing checkout in ${LIBKRUN_CACHE_DIR}..."
    cd "$LIBKRUN_CACHE_DIR"
    git fetch origin "$LIBKRUN_TAG" 2>/dev/null || true
    git checkout "$LIBKRUN_TAG" 2>/dev/null || {
        err "Failed to checkout ${LIBKRUN_TAG} in existing cache. Removing and re-cloning."
        rm -rf "$LIBKRUN_CACHE_DIR"
    }
fi

if [ ! -d "$LIBKRUN_CACHE_DIR" ]; then
    info "Cloning nanosandbox libkrun (${LIBKRUN_TAG})..."
    git clone --depth=1 --branch "$LIBKRUN_TAG" "$LIBKRUN_REPO" "$LIBKRUN_CACHE_DIR"
fi

cd "$LIBKRUN_CACHE_DIR"

# Verify the pinned SHA
ACTUAL_SHA=$(git rev-parse HEAD)
if [ "$ACTUAL_SHA" != "$LIBKRUN_SHA" ]; then
    err "SHA mismatch! Expected ${LIBKRUN_SHA}, got ${ACTUAL_SHA}"
    err "The upstream tag ${LIBKRUN_TAG} has changed. Update LIBKRUN_SHA in this script."
    exit 1
fi
ok "SHA verified: ${ACTUAL_SHA}"

# 2a. The next-mode init changes (extra virtiofs mounts + static network) are
#     committed on the fork's `nanosandbox` branch — no build-time patching.
ok "Customizations are committed in the fork (${LIBKRUN_TAG}@${LIBKRUN_SHA:0:8})"

# 2b. Apply musl statx patch only for musl targets
if [ -n "${MUSL_TARGET:-}" ] || (command -v apk >/dev/null 2>&1); then
    info "Applying musl statx compatibility patch..."
    PASSTHROUGH="$LIBKRUN_CACHE_DIR/src/devices/src/virtio/fs/linux/passthrough.rs"
    if [ -f "$PASSTHROUGH" ]; then
        python3 "$SCRIPT_DIR/ci/patch-libkrun-musl-statx.py" "$PASSTHROUGH"
        ok "musl statx patch applied"
    else
        err "passthrough.rs not found at ${PASSTHROUGH}"
        exit 1
    fi
fi

# 3. Build as staticlib
info "Building libkrun (staticlib) with features: ${LIBKRUN_FEATURES}..."

# Temporarily modify crate-type to staticlib
CARGO_TOML="$LIBKRUN_CACHE_DIR/src/libkrun/Cargo.toml"
sed -i.bak 's/crate-type = \["cdylib", "lib"\]/crate-type = ["staticlib"]/' "$CARGO_TOML"

# Set up environment for cross-compilation
export LIBCLANG_PATH="${LIBCLANG_PATH:-$(brew --prefix llvm 2>/dev/null || echo "")/lib}"
export NET=1
export BLK=1
export INIT_BLOB=1

# On macOS, the Makefile downloads a Debian sysroot for cross-compiling the init blob.
# We use the Makefile's sysroot target to set this up, then build with cargo.
if [ "$(uname)" = "Darwin" ]; then
    info "Preparing Linux sysroot for init_blob cross-compilation..."
    make -j"$JOBS" linux-sysroot/.sysroot_ready 2>&1 | tail -5

    # Source the CC_LINUX from the Makefile
    GCC_TRIPLET="aarch64-linux-gnu"
    CLANG="$(brew --prefix llvm)/bin/clang"
    SYSROOT_LINUX="$LIBKRUN_CACHE_DIR/linux-sysroot"
    GCC_LIB_DIR="$SYSROOT_LINUX/usr/lib/aarch64-linux-gnu"
    export CC_LINUX="$CLANG -target $GCC_TRIPLET -fuse-ld=lld -Wl,-strip-debug --sysroot $SYSROOT_LINUX -B$GCC_LIB_DIR -L$GCC_LIB_DIR -Wno-c23-extensions"
fi

# Build the staticlib using cargo (with CC_LINUX set for init_blob cross-compilation)
cargo build --release --features "$LIBKRUN_FEATURES" -j "$JOBS" 2>&1

# Restore the original Cargo.toml
mv "$CARGO_TOML.bak" "$CARGO_TOML"

# 4. Copy the static library to the output directory
mkdir -p "$LIBKRUN_OUTPUT_DIR"
cp "target/release/libkrun.a" "$LIBKRUN_OUTPUT_DIR/"
ok "libkrun.a copied to ${LIBKRUN_OUTPUT_DIR}/"

# 5. Report
LIBKRUN_A_SIZE=$(stat -f%z "$LIBKRUN_OUTPUT_DIR/libkrun.a" 2>/dev/null || stat -c%s "$LIBKRUN_OUTPUT_DIR/libkrun.a" 2>/dev/null || echo "?")
info "Build complete:"
echo "  Artifact: ${LIBKRUN_OUTPUT_DIR}/libkrun.a"
echo "  Size:     ${LIBKRUN_A_SIZE} bytes ($(( LIBKRUN_A_SIZE / 1024 )) KB)"
echo "  Tag:      ${LIBKRUN_TAG}"
echo "  SHA:      ${ACTUAL_SHA}"
echo ""
echo "To use in nanosandbox builds, set:"
echo "  export LIBKRUN_LIB_DIR=${LIBKRUN_OUTPUT_DIR}"
