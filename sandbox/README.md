# Nanosandbox SDK

**Agent-aware sandbox management and multi-language SDK interface**

## Overview

Wraps the [nanosandbox](https://github.com/nanosandboxai/runtime) runtime crate with agent-specific functionality. This is the SDK interface that other language SDKs bind to via C FFI. It provides sandbox lifecycle management, `sandbox.yml` configuration, and host-side config delivery (MCP servers, skills, agent commands) via the `deploy` module.

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

- **`AgentSandbox`** -- Agent-aware wrapper around `nanosandbox::Sandbox`. Provides lifecycle operations (create, destroy) and console-based messaging.
- **`AgentSandboxConfig`** -- Extends the runtime `SandboxConfig` with agent-specific fields: MCP servers, skills, permissions, agent type, model selection, and auto mode.
- **`deploy`** -- Host-side config delivery module. Generates mount plans, MCP config files, skills files, and agent commands at deploy time. Replaces the in-VM Go gateway's config generation.
- **`Session`** -- Session persistence supporting resume, fresh, and destroy workflows.
- **`config::file`** -- `sandbox.yml` parsing, resolution, and CLI override application.
- **`AgentsRegistryClient`** -- Fetches agent definitions and skill content from the OCI-based agents registry (`ghcr.io/nanosandboxai/agents-registry`).

## Config Delivery Model (Phase 4+)

All agent configuration is delivered at deploy time via the `deploy` module:

1. **Mount planner** (`deploy::MountPlanner`) -- Computes virtiofs mounts: workspace RW and one merged per-agent RW mount per guest path (generated config files are written into the state dirs so each path has a single mount).
2. **MCP config generation** (`deploy::ConfigGenerator`) -- Generates per-agent MCP server config files (Claude JSON, Goose YAML, Codex TOML, Cursor JSON).
3. **Skills generation** (`deploy::SkillsGenerator`) -- Generates skill/prompt files per agent format (SKILL.md, .goosehints, .mdc rules).
4. **Agent command builder** (`deploy::AgentCommandBuilder`) -- Builds the CLI invocation (binary + args) for each agent type.
5. **Secrets** -- Passed via in-memory boot env (`krun_set_env`); never written to disk.

No in-VM CRUD APIs remain. Config is regenerated host-side on every deploy and merged into the agent state mounts.

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
│           ├── agent_sandbox.rs    # AgentSandbox wrapper (lifecycle only)
│           ├── deploy/             # Host-side config delivery
│           │   ├── mod.rs          # DeployPlanner orchestration
│           │   ├── mount_planner.rs # virtiofs mount planning
│           │   ├── config_gen.rs   # MCP config generation
│           │   ├── skills_gen.rs   # Skills/prompt generation
│           │   └── agent_cmd.rs    # Agent command + env building
│           ├── config/
│           │   ├── mod.rs          # Agent config types (AgentSandboxConfig, Permissions, AgentType)
│           │   ├── file.rs         # sandbox.yml parsing and resolution
│           │   └── models.rs       # Model validation
│           ├── agents_registry.rs  # OCI-based agent registry client
│           ├── session.rs          # Session persistence
│           ├── settings.rs         # User settings
│           └── error.rs            # Error types
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
