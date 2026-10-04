# Nanosandbox SDK

**Agent-aware sandbox management and multi-language SDK interface**

## Overview

Wraps the [nanosandbox](https://github.com/nanosandboxai/runtime) runtime crate with agent-specific functionality. This is the SDK interface that other language SDKs bind to via C FFI. It provides agent lifecycle management, MCP server management, skill injection, session persistence, and `sandbox.yml` configuration.

## Architecture

```
Language SDKs (Python, Node, Go)
       | (C FFI bindings)
   Sandbox SDK (this repo)
       | (Rust dependency)
   Nanosandbox Runtime
       |
   libkrun (microVM)
       |
   Hardware Virtualization
```

## Key Components

- **`AgentSandbox`** -- Agent-aware wrapper around `nanosandbox::Sandbox`. Adds operations for messaging, bootstrapping, MCP server management, and skill injection via the in-VM gateway HTTP API.
- **`AgentSandboxConfig`** -- Extends the runtime `SandboxConfig` with agent-specific fields: MCP servers, skills, permissions, agent type, model selection, and auto mode.
- **`Session`** -- Session persistence supporting resume, fresh, and destroy workflows.
- **`config::file`** -- `sandbox.yml` parsing, resolution, and CLI override application.
- **`AgentsRegistryClient`** -- Fetches agent definitions and skill content from the OCI-based agents registry (`ghcr.io/nanosandboxai/agents-registry`).
- **`agent-gateway/`** -- Go binary that runs inside the microVM, handling agent orchestration, MCP server lifecycle, and skill delivery over a local HTTP API.

## FFI Bindings

The sandbox crate exposes C ABI functions for multi-language SDK support:

- Build with `cargo build --release --features ffi` to produce `libnanosandbox_sdk.so` (Linux) or `libnanosandbox_sdk.dylib` (macOS).
- Language SDKs in Python, Node, and Go bind to these exported functions.

## Crate Structure

```
sandbox/
├── Cargo.toml                      # Workspace root
├── crates/
│   └── sandbox/
│       └── src/
│           ├── lib.rs              # Re-exports + public API surface
│           ├── agent_sandbox.rs    # AgentSandbox wrapper
│           ├── config/
│           │   ├── mod.rs          # Agent config types (AgentSandboxConfig, Permissions, AgentType)
│           │   ├── file.rs         # sandbox.yml parsing and resolution
│           │   └── models.rs       # Model validation
│           ├── agents_registry.rs  # OCI-based agent registry client
│           ├── session.rs          # Session persistence
│           ├── settings.rs         # User settings
│           └── error.rs            # Error types
├── agent-gateway/                  # Go binary (in-VM agent orchestration)
│   ├── main.go
│   ├── mcp/                       # MCP server management
│   └── skills/                    # Skill injection
└── docs/
    └── sdk-bindings.md             # FFI documentation
```

## Build Instructions

```bash
# Build the Rust SDK
cargo build -p sandbox

# Run tests
cargo test -p sandbox

# Build as shared library (for FFI)
cargo build --release -p sandbox --features ffi

# Build agent-gateway
cd agent-gateway && go build -o agent-gateway .
```

## Supported Agent Types

The SDK supports the following agent types, selectable via `sandbox.yml` or CLI flags:

| Agent     | Identifier    |
|-----------|---------------|
| Claude    | `claude`      |
| Codex     | `codex`       |
| Goose     | `goose`       |
| Cursor    | `cursor`      |

## Related Repos

- [Runtime](https://github.com/nanosandboxai/runtime) -- Pure VM engine (libkrun FFI, OCI images, containerization)
- [Agents Registry](https://github.com/nanosandboxai/agents-registry) -- Agent definitions, Docker images
- [Nanosandbox monorepo](https://github.com/nanosandboxai/nanosandbox) -- CLI, runtime, agent-gateway, registry, and releases

## License

Apache-2.0
