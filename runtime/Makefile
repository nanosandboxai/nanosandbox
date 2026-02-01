# Nanosandbox Makefile
#
# Common targets for building, testing, and development

.PHONY: help build build-release test test-unit test-e2e test-e2e-network \
        install-runtime check-prereqs clean clippy fmt lint

# Default target
help:
	@echo "Nanosandbox Build System"
	@echo ""
	@echo "Build targets:"
	@echo "  build           Build in debug mode"
	@echo "  build-release   Build in release mode"
	@echo "  build-cli       Build CLI binary"
	@echo ""
	@echo "Test targets:"
	@echo "  test            Run unit and integration tests"
	@echo "  test-unit       Run only unit tests"
	@echo "  test-e2e        Run all E2E tests (requires runtime)"
	@echo "  test-e2e-network Run network-only E2E tests"
	@echo ""
	@echo "Setup targets:"
	@echo "  install-runtime Install crun+libkrun for E2E tests"
	@echo "  check-prereqs   Check E2E test prerequisites"
	@echo ""
	@echo "Development targets:"
	@echo "  clippy          Run Clippy linter"
	@echo "  fmt             Format code"
	@echo "  lint            Run all linters (clippy + fmt check)"
	@echo "  clean           Clean build artifacts"

# =============================================================================
# Build Targets
# =============================================================================

build:
	cargo build

build-release:
	cargo build --release

build-cli:
	cargo build --features cli

# =============================================================================
# Test Targets
# =============================================================================

# Run all tests (including E2E - requires network)
test:
	cargo test --features cli

# Run only unit tests (lib tests)
test-unit:
	cargo test --lib

# Run all tests with verbose output
test-verbose:
	cargo test --features cli -- --nocapture

# =============================================================================
# Setup Targets
# =============================================================================

# Install crun + libkrun runtime
install-runtime:
	./scripts/install-runtime.sh

# Check prerequisites for E2E tests
check-prereqs:
	@./scripts/check-e2e-prereqs.sh

# Check prerequisites for network-only tests
check-prereqs-network:
	@./scripts/check-e2e-prereqs.sh --network-only

# =============================================================================
# Development Targets
# =============================================================================

# Run Clippy linter
clippy:
	cargo clippy --features cli -- -D warnings

# Format code
fmt:
	cargo fmt

# Check formatting without changes
fmt-check:
	cargo fmt -- --check

# Run all linters
lint: clippy fmt-check

# Clean build artifacts
clean:
	cargo clean

# =============================================================================
# CI Targets
# =============================================================================

# Run full CI pipeline locally
ci: lint test test-e2e-network

# Run CI without E2E (faster)
ci-fast: lint test
