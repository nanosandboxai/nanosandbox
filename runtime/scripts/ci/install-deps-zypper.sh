#!/bin/bash
# Install build dependencies on openSUSE (zypper)
set -euo pipefail

zypper --non-interactive install \
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
    libelf-devel \
    pkg-config \
    libopenssl-devel \
    cpio \
    patch \
    diffutils \
    gawk \
    clang-devel \
    glibc-devel-static \
    rust \
    cargo

# Install Go from upstream (distro Go may be too old for deps)
if ! command -v go >/dev/null 2>&1 || ! go version 2>/dev/null | grep -qE 'go1\.(2[4-9]|[3-9])'; then
    curl -fsSL "https://go.dev/dl/go1.24.0.linux-amd64.tar.gz" | tar -C /usr/local -xzf -
    export PATH="/usr/local/go/bin:$PATH"
    echo 'export PATH="/usr/local/go/bin:$PATH"' >> "$HOME/.bashrc"
fi

echo "Build dependencies installed (zypper)"
