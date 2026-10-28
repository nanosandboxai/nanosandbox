#!/usr/bin/env bash
# Fetch and verify the pinned Linux kernel tarball used by libkrunfw.
#
# Epic 8 (#143) T3 (#146): make the kernel download deterministic by pinning
# the URL and sha256. The libkrunfw Makefile only downloads when the tarball
# is absent, so pre-populating a verified tarball makes the build reproducible
# and offline-capable after the first fetch.
#
# Usage: fetch-kernel-tarball.sh <libkrunfw-dir>
set -euo pipefail

DIR="${1:?usage: fetch-kernel-tarball.sh <libkrunfw-dir>}"
VERSION="linux-6.12.76"
URL="https://cdn.kernel.org/pub/linux/kernel/v6.x/${VERSION}.tar.xz"
SHA256="bbb43e834c46e6bd49a5c28f22e679a937443404e1f653204d4b24929f3ad896"
TARBALL="${DIR}/tarballs/${VERSION}.tar.xz"

verify() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        echo "${SHA256}  ${file}" | sha256sum -c -
    else
        echo "${SHA256}  ${file}" | shasum -a 256 -c -
    fi
}

mkdir -p "${DIR}/tarballs"

if [ -f "${TARBALL}" ] && verify "${TARBALL}" >/dev/null 2>&1; then
    echo "==> kernel tarball verified: ${TARBALL}"
    exit 0
fi

echo "==> fetching ${URL}"
curl -fsSL "${URL}" -o "${TARBALL}.tmp"
verify "${TARBALL}.tmp"
mv "${TARBALL}.tmp" "${TARBALL}"
echo "==> kernel tarball pinned and verified: ${TARBALL}"
