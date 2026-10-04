#!/bin/bash
# Install build dependencies on Arch Linux (pacman)
set -euo pipefail

pacman -Sy --noconfirm \
    base-devel \
    curl \
    ca-certificates \
    git \
    python \
    python-pyelftools \
    bc \
    flex \
    bison \
    pkg-config \
    openssl \
    cpio \
    clang \
    rust \
    go

# Verify Go >= 1.24 (Arch rolling should have it, fallback to upstream)
if ! go version 2>/dev/null | grep -qE 'go1\.(2[4-9]|[3-9])'; then
    curl -fsSL "https://go.dev/dl/go1.24.0.linux-amd64.tar.gz" | tar -C /usr/local -xzf -
    export PATH="/usr/local/go/bin:$PATH"
    echo 'export PATH="/usr/local/go/bin:$PATH"' >> "$HOME/.bashrc"
fi

echo "Build dependencies installed (pacman)"
