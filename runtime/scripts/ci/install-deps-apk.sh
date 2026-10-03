#!/bin/sh
# Install build dependencies on Alpine Linux (apk / musl libc)
set -eu

apk update
apk add --no-cache \
    build-base \
    curl \
    ca-certificates \
    git \
    python3 \
    py3-elftools \
    bc \
    flex \
    bison \
    elfutils-dev \
    pkgconf \
    openssl-dev \
    openssl-libs-static \
    cpio \
    linux-headers \
    ncurses-dev \
    perl \
    diffutils \
    findutils \
    bash \
    clang-dev

# Install Rust via rustup (Alpine's apk rust package is too old for edition2024)
if ! command -v rustc >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
    . "$HOME/.cargo/env"
fi

# Install Go from upstream (distro Go may be too old for deps)
if ! command -v go >/dev/null 2>&1 || ! go version 2>/dev/null | grep -qE 'go1\.(2[4-9]|[3-9])'; then
    curl -fsSL "https://go.dev/dl/go1.24.0.linux-amd64.tar.gz" | tar -C /usr/local -xzf -
    export PATH="/usr/local/go/bin:$PATH"
    echo 'export PATH="/usr/local/go/bin:$PATH"' >> "$HOME/.bashrc"
fi

echo "Build dependencies installed (apk/musl)"
