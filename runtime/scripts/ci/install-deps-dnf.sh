#!/bin/bash
# Install build dependencies on Fedora (dnf)
set -euo pipefail

dnf install -y \
    gcc \
    make \
    curl \
    ca-certificates \
    git \
    python3 \
    python3-pyelftools \
    bc \
    flex \
    bison \
    elfutils-libelf-devel \
    pkg-config \
    openssl-devel \
    cpio \
    patch \
    clang-devel \
    glibc-static

# Install Rust via rustup (Fedora's distro rust is too old for libkrun deps)
if ! command -v rustc >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
    source "$HOME/.cargo/env"
fi

# Install Go from upstream (distro Go may be too old for deps)
if ! command -v go >/dev/null 2>&1 || ! go version 2>/dev/null | grep -qE 'go1\.(2[4-9]|[3-9])'; then
    curl -fsSL "https://go.dev/dl/go1.24.0.linux-amd64.tar.gz" | tar -C /usr/local -xzf -
    export PATH="/usr/local/go/bin:$PATH"
    echo 'export PATH="/usr/local/go/bin:$PATH"' >> "$HOME/.bashrc"
fi

echo "Build dependencies installed (dnf)"
