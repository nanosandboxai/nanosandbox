#!/bin/bash
#
# Check prerequisites for running Nanosandbox E2E tests
#
# Usage: ./scripts/check-e2e-prereqs.sh [--network-only]
#
# Exit codes:
#   0 - All prerequisites met
#   1 - Missing prerequisites

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

passed=0
failed=0
warnings=0

pass() {
    echo -e "${GREEN}[PASS]${NC} $1"
    ((passed++)) || true
}

fail() {
    echo -e "${RED}[FAIL]${NC} $1"
    ((failed++)) || true
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
    ((warnings++)) || true
}

info() {
    echo -e "       $1"
}

network_only=false
if [[ "$1" == "--network-only" ]]; then
    network_only=true
fi

OS="$(uname -s)"
ARCH="$(uname -m)"

echo "=========================================="
echo "Nanosandbox E2E Prerequisites Check"
echo "=========================================="
echo ""
echo "Platform: $OS ($ARCH)"
echo ""

# =============================================================================
# Rust Toolchain
# =============================================================================
echo "--- Rust Toolchain ---"

if command -v rustc &> /dev/null; then
    version=$(rustc --version)
    pass "Rust compiler: $version"
else
    fail "Rust compiler not found"
    info "Install from: https://rustup.rs"
fi

if command -v cargo &> /dev/null; then
    version=$(cargo --version)
    pass "Cargo: $version"
else
    fail "Cargo not found"
fi

echo ""

# =============================================================================
# Network Connectivity
# =============================================================================
echo "--- Network Connectivity ---"

# Test Docker Hub registry
if curl -s --connect-timeout 5 "https://registry-1.docker.io/v2/" &> /dev/null; then
    pass "Docker Hub registry reachable"
else
    fail "Cannot reach Docker Hub registry (registry-1.docker.io)"
    info "Check your internet connection or firewall settings"
fi

# Test GHCR
if curl -s --connect-timeout 5 "https://ghcr.io/v2/" &> /dev/null; then
    pass "GitHub Container Registry reachable"
else
    warn "Cannot reach GitHub Container Registry (ghcr.io)"
fi

echo ""

# If network-only mode, skip runtime checks
if $network_only; then
    echo "--- Skipping Runtime Checks (--network-only) ---"
    echo ""
else
    # =============================================================================
    # Container Runtime
    # =============================================================================
    echo "--- Container Runtime ---"

    runtime_found=false

    # Check for krunvm (macOS)
    if command -v krunvm &> /dev/null; then
        version=$(krunvm --version 2>&1 | head -1)
        pass "krunvm found: $version"
        runtime_found=true
    fi

    # Check for krun (OCI runtime)
    if command -v krun &> /dev/null; then
        version=$(krun --version 2>&1 | head -1)
        pass "krun found: $version"
        runtime_found=true
    fi

    # Check for crun with libkrun
    if command -v crun &> /dev/null; then
        version=$(crun --version 2>&1 | head -1)
        if crun --version 2>&1 | grep -q "libkrun"; then
            pass "crun with libkrun: $version"
            runtime_found=true
        else
            warn "crun found but without libkrun support: $version"
            info "Rebuild crun with --with-libkrun or install krunvm"
        fi
    fi

    if ! $runtime_found; then
        fail "No compatible runtime found (need krunvm, crun+libkrun, or krun)"
        info "Run: ./scripts/install-runtime.sh"
    fi

    echo ""

    # =============================================================================
    # Hypervisor
    # =============================================================================
    echo "--- Hypervisor ---"

    case "$OS" in
        Darwin)
            # Check for Hypervisor.framework on macOS
            if [[ "$ARCH" == "arm64" ]]; then
                # Check if HVF is available
                if sysctl -n kern.hv_support 2>/dev/null | grep -q "1"; then
                    pass "Hypervisor.framework (HVF) available"
                else
                    fail "Hypervisor.framework not available"
                    info "Apple Silicon Macs should have HVF by default"
                fi
                
                # Check krun entitlement if installed
                if command -v krun &> /dev/null; then
                    if codesign -d --entitlements :- "$(which krun)" 2>&1 | grep -q "com.apple.security.hypervisor"; then
                        pass "krun has HVF entitlement"
                    else
                        warn "krun may not have HVF entitlement"
                    fi
                fi
            else
                fail "macOS x86_64 not supported (need Apple Silicon)"
            fi
            ;;
        Linux)
            # Check for KVM
            if [[ -e /dev/kvm ]]; then
                pass "KVM device available (/dev/kvm)"
                
                # Check permissions
                if [[ -r /dev/kvm ]] && [[ -w /dev/kvm ]]; then
                    pass "KVM device accessible"
                else
                    fail "Cannot access /dev/kvm"
                    info "Add user to kvm group: sudo usermod -aG kvm $USER"
                fi
            else
                fail "KVM not available (/dev/kvm not found)"
                info "Enable KVM: sudo modprobe kvm && sudo modprobe kvm_intel (or kvm_amd)"
            fi
            ;;
        *)
            fail "Unsupported OS: $OS"
            ;;
    esac

    echo ""
fi

# =============================================================================
# Summary
# =============================================================================
echo "=========================================="
echo "Summary"
echo "=========================================="
echo -e "${GREEN}Passed:${NC}   $passed"
echo -e "${YELLOW}Warnings:${NC} $warnings"
echo -e "${RED}Failed:${NC}   $failed"
echo ""

if [[ $failed -gt 0 ]]; then
    echo -e "${RED}Some prerequisites are missing.${NC}"
    echo ""
    echo "To install runtime: ./scripts/install-runtime.sh"
    echo "To run network-only tests: make test-e2e-network"
    exit 1
else
    if [[ $warnings -gt 0 ]]; then
        echo -e "${YELLOW}All required prerequisites met (with warnings).${NC}"
    else
        echo -e "${GREEN}All prerequisites met!${NC}"
    fi
    echo ""
    echo "You can now run: make test-e2e"
    exit 0
fi
