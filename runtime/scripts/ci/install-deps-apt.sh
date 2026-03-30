#!/bin/bash
# Install build dependencies on Ubuntu/Debian (apt-get)
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive

apt-get update
apt-get install -y --no-install-recommends \
    build-essential \
    curl \
    ca-certificates \
    git \
    python3 \
    python3-pyelftools \
    bc \
    flex \
    bison \
    libelf-dev \
    pkg-config \
    libssl-dev \
    cpio \
    libclang-dev

# Install Go from upstream (distro golang-go is too old for deps)
if ! command -v go >/dev/null 2>&1 || ! go version 2>/dev/null | grep -qE 'go1\.(2[4-9]|[3-9])'; then
    curl -fsSL "https://go.dev/dl/go1.24.0.linux-amd64.tar.gz" | tar -C /usr/local -xzf -
    export PATH="/usr/local/go/bin:$PATH"
    echo 'export PATH="/usr/local/go/bin:$PATH"' >> "$HOME/.bashrc"
fi

# Install Rust via rustup
if ! command -v rustc &>/dev/null; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi

echo "Build dependencies installed (apt)"
