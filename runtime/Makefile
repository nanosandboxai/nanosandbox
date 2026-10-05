# Nanosandbox Makefile
#
# Common targets for building, testing, and development

.PHONY: help build build-release test test-unit test-e2e test-e2e-network \
        install-runtime check-prereqs clean clippy fmt lint \
        registry-up registry-down agents-build-base agents-build agents-build-one \
        agents-test agents-list agents-e2e

# Default target
help:
	@echo "Nanosandbox Build System"
	@echo ""
	@echo "Build targets:"
	@echo "  build-libkrun   Build upstream libkrun v1.19.5 as static library"
	@echo "  build           Build in debug mode"
	@echo "  build-release   Build in release mode"
	@echo ""
	@echo "Test targets:"
	@echo "  test            Run unit and integration tests"
	@echo "  test-unit       Run only unit tests"
	@echo "  test-e2e        Run all E2E tests (requires runtime)"
	@echo "  test-e2e-network Run network-only E2E tests"
	@echo ""
	@echo "Setup targets:"
	@echo "  install-runtime Install libkrun for E2E tests"
	@echo "  check-prereqs   Check E2E test prerequisites"
	@echo ""
	@echo "Development targets:"
	@echo "  clippy          Run Clippy linter"
	@echo "  fmt             Format code"
	@echo "  lint            Run all linters (clippy + fmt check)"
	@echo "  clean           Clean build artifacts"
	@echo ""
	@echo "Agent Docker targets:"
	@echo "  registry-up       Start local Docker registry on localhost:5050"
	@echo "  registry-down     Stop local Docker registry"
	@echo "  agents-build-base Build and push shared base image"
	@echo "  agents-build      Build and push all agent images (includes base)"
	@echo "  agents-build-one  Build single agent (AGENT=claude|goose|codex|cursor)"
	@echo "  agents-test       Smoke test all agent images (--version)"
	@echo "  agents-list       Show images in local registry"
	@echo "  agents-e2e        Run full E2E tests with nanosandbox"

# =============================================================================
# Build Targets
# =============================================================================

build-libkrun:
	./scripts/build-libkrun.sh

build:
	cargo build -p runtime

build-release:
	cargo build --release -p runtime

# =============================================================================
# Test Targets
# =============================================================================

# Run all tests for the runtime package
test:
	cargo test -p runtime

# Run only unit tests (lib tests)
test-unit:
	cargo test -p runtime --lib

# Run all tests with verbose output
test-verbose:
	cargo test -p runtime -- --nocapture

# =============================================================================
# Setup Targets
# =============================================================================

# Install runtime dependencies (platform-specific)
install-runtime:
ifeq ($(shell uname),Darwin)
	./scripts/install/macos.sh
else
	./scripts/install/linux.sh
endif

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
	cargo clippy -p runtime -p libkrun-sys -- -D warnings

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

# =============================================================================
# Agent Docker Image Targets
# =============================================================================

REGISTRY_HOST ?= localhost:5050
BASE_IMAGE_TAG ?= latest
AGENTS := claude goose codex cursor
AGENTS_DIR := ../agents-registry
COMPOSE_FILE := $(AGENTS_DIR)/docker/docker-compose.yml

# Start local Docker registry
registry-up:
	@docker compose -f $(COMPOSE_FILE) up -d
	@echo "Registry running at $(REGISTRY_HOST)"
	@echo "Waiting for registry..."
	@until curl -sf http://$(REGISTRY_HOST)/v2/_catalog > /dev/null 2>&1; do sleep 1; done
	@echo "Registry ready"

# Stop local Docker registry
registry-down:
	@docker compose -f $(COMPOSE_FILE) down

# Build and push base image to local registry
# --load ensures buildx loads the image into the local daemon so push works
agents-build-base:
	docker build --load -f $(AGENTS_DIR)/docker/Dockerfile.base -t $(REGISTRY_HOST)/agents-registry/base:$(BASE_IMAGE_TAG) $(AGENTS_DIR)
	docker push $(REGISTRY_HOST)/agents-registry/base:$(BASE_IMAGE_TAG)
	@echo "Base image pushed to $(REGISTRY_HOST)/agents-registry/base:$(BASE_IMAGE_TAG)"

# Build and push a single agent (usage: make agents-build-one AGENT=claude)
agents-build-one:
ifndef AGENT
	$(error AGENT is required. Usage: make agents-build-one AGENT=claude)
endif
	docker build --load -f $(AGENTS_DIR)/docker/Dockerfile.$(AGENT) \
		--build-arg BASE_IMAGE=$(REGISTRY_HOST)/agents-registry/base:$(BASE_IMAGE_TAG) \
		-t $(REGISTRY_HOST)/agents-registry/$(AGENT):$(BASE_IMAGE_TAG) $(AGENTS_DIR)
	docker push $(REGISTRY_HOST)/agents-registry/$(AGENT):$(BASE_IMAGE_TAG)
	@echo "$(AGENT) image pushed to $(REGISTRY_HOST)/agents-registry/$(AGENT):$(BASE_IMAGE_TAG)"

# Build and push all agent images (builds base first)
agents-build: agents-build-base
	@for agent in $(AGENTS); do \
		echo "Building $$agent..."; \
		docker build --load -f $(AGENTS_DIR)/docker/Dockerfile.$$agent \
			--build-arg BASE_IMAGE=$(REGISTRY_HOST)/agents-registry/base:$(BASE_IMAGE_TAG) \
			-t $(REGISTRY_HOST)/agents-registry/$$agent:$(BASE_IMAGE_TAG) $(AGENTS_DIR) \
		&& docker push $(REGISTRY_HOST)/agents-registry/$$agent:$(BASE_IMAGE_TAG) \
		|| exit 1; \
	done
	@echo "All agent images built and pushed"

# Smoke test: verify each agent image runs --version
agents-test:
	@for agent in $(AGENTS); do \
		echo "Testing $$agent..."; \
		docker run --rm $(REGISTRY_HOST)/agents-registry/$$agent:$(BASE_IMAGE_TAG) $$agent --version \
		&& echo "  $$agent: OK" \
		|| echo "  $$agent: FAILED"; \
	done

# Show local registry contents
agents-list:
	@curl -sf http://$(REGISTRY_HOST)/v2/_catalog | python3 -m json.tool 2>/dev/null \
		|| curl -sf http://$(REGISTRY_HOST)/v2/_catalog

# Full E2E test with nanosandbox
agents-e2e:
	cargo test -p runtime --test agents_e2e_test -- --ignored --nocapture
