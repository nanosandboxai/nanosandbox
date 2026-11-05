ARG BASE_IMAGE=ghcr.io/nanosandboxai/nanosandbox/base:latest
FROM ${BASE_IMAGE}

LABEL org.opencontainers.image.description="Goose agent for nanosandbox sandboxes"

# Pinned to a specific GitHub release. NOT Dependabot-managed (no npm
# package); bump manually in this Dockerfile.
ARG GOOSE_VERSION=0.86.0

# Install bzip2 (tarball extraction) and libgomp1 (OpenMP runtime required by goose)
RUN /usr/bin/apt-get update && /usr/bin/apt-get install -y --no-install-recommends bzip2 libgomp1 \
    && rm -rf /var/lib/apt/lists/*

ARG TARGETARCH

# Download pre-built glibc binary from GitHub releases (works natively on Debian)
RUN set -eux; \
    case "${TARGETARCH}" in \
        amd64) GOOSE_ARCH="x86_64-unknown-linux-gnu" ;; \
        arm64) GOOSE_ARCH="aarch64-unknown-linux-gnu" ;; \
        *) echo "Unsupported architecture: ${TARGETARCH}" && exit 1 ;; \
    esac; \
    curl -fsSL -o /tmp/goose.tar.bz2 \
        "https://github.com/block/goose/releases/download/v${GOOSE_VERSION}/goose-${GOOSE_ARCH}.tar.bz2" \
    && tar -xjf /tmp/goose.tar.bz2 -C /tmp \
    && mv /tmp/goose /usr/local/bin/goose \
    && chmod +x /usr/local/bin/goose \
    && rm -rf /tmp/goose.tar.bz2

RUN goose --version && echo "Goose: OK"
