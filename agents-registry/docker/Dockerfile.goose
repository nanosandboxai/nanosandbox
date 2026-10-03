ARG BASE_IMAGE=agents-registry/base:latest
FROM ${BASE_IMAGE}

LABEL org.opencontainers.image.description="Goose agent for nanosandbox sandboxes"

# Install bzip2 (tarball extraction) and libgomp1 (OpenMP runtime required by goose)
# Use /usr/bin/apt-get directly to bypass the rootless wrapper during build.
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
        "https://github.com/block/goose/releases/latest/download/goose-${GOOSE_ARCH}.tar.bz2" \
    && tar -xjf /tmp/goose.tar.bz2 -C /tmp \
    && mv /tmp/goose /usr/local/bin/goose \
    && chmod +x /usr/local/bin/goose \
    && rm -rf /tmp/goose.tar.bz2

RUN goose --version && echo "Goose: OK"
RUN rm -rf /workspace/*

CMD ["/usr/local/bin/nanosb-init.sh"]
