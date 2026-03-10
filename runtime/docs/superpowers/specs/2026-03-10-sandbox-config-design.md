# Sandbox Config File Design

## Overview

Project-level `sandbox.yml` files that define sandbox configurations for automatic detection and startup. When `nanosb` is invoked in a directory containing `sandbox.yml`, it parses the config and auto-starts all defined sandboxes as TUI panels.

## Config File Format

**File name**: `sandbox.yml` or `sandbox.yaml`

**Top-level structure**:

```yaml
defaults:
  # Shared configuration inherited by all sandboxes
  image: nanosb-claude:latest
  cpus: 2
  memory: 4096

sandboxes:
  claude:
    name: my-claude-sandbox
    mcp:
      github:
        command: npx
        args: [-y, "@modelcontextprotocol/server-github"]
  codex:
    image: nanosb-codex:latest
    cpus: 4
```

## Complete Field Reference

All fields are optional at every level. `image` must resolve to a value after merge.

```yaml
defaults:
  image: nanosb-claude:latest       # OCI image reference
  cpus: 2                           # CPU cores (default: 1)
  memory: 4096                      # Memory in MB (default: 512)
  timeout: 600                      # Timeout in seconds (default: 300)
  workdir: /workspace               # Working directory inside VM

  env:
    KEY: value
    SECRET: ${HOST_ENV_VAR}         # expand from host environment

  network:
    enabled: true
    mode: tsi                       # tsi | bridge | none
    scope: any                      # any | public | group | none
    ports:
      - "8080:80"                   # host:container
      - "3000:3000/udp"             # with protocol
    dns:
      - 8.8.8.8

  mounts:
    - host: ./data
      container: /data
      readonly: false
      type: virtiofs                # bind | virtiofs (default: bind)

  mcp:
    github:
      command: npx
      args: [-y, "@modelcontextprotocol/server-github"]
      env:
        GITHUB_TOKEN: ${GITHUB_TOKEN}
      enabled: true

  project:
    path: .                         # host path, relative to sandbox.yml location
    branch: my-branch               # auto-generated if omitted
    mount_point: /workspace
    auto_sync: false

sandboxes:
  claude:
    name: claude-dev                # optional display name (defaults to map key)
    # ... any field from above, overrides defaults
```

## Merge Order

Later wins:

1. Hardcoded defaults (`SandboxConfig::default()`)
2. `defaults:` block in `sandbox.yml`
3. Per-sandbox fields in `sandbox.yml`
4. CLI flags

Env vars and MCP servers **merge** (union of keys, per-sandbox wins on conflict). Mounts and ports **replace** (per-sandbox list replaces defaults entirely).

## Name Parameter

- `name` is an optional user-friendly alias; internal UUID remains the system ID
- Sandbox map key is the default name; explicit `name:` field overrides it
- CLI: `--name` flag on `nanosb run` and `/add` TUI command
- Lookup: `nanosb stop my-sandbox` works alongside `nanosb stop <uuid>`
- `nanosb ps` shows both: `ID | NAME | IMAGE | STATUS`
- Uniqueness enforced among running sandboxes
- Validation: lowercase alphanumeric + hyphens, max 64 chars

## CLI Detection & Auto-Start

1. `nanosb` starts (TUI mode, no explicit subcommand)
2. Checks CWD for `sandbox.yml` / `sandbox.yaml`
3. Parses and validates config
4. Launches TUI with all sandboxes auto-starting as panels

**Flag interactions**:

- `nanosb` (no flags, config found) -> start all sandboxes from config
- `nanosb --sandbox claude` -> start only `claude` from config
- `nanosb --cpus 8` -> override cpus for all sandboxes
- `nanosb --sandbox claude --cpus 8` -> start `claude` only with 8 cpus
- `nanosb run --image foo echo hello` -> explicit subcommand, ignores sandbox.yml
- No config found -> current behavior (empty TUI, manual `/add`)

**New flag**: `--config <path>` to specify config file/directory path.

## Multi-Config Composition

Multiple `--config` flags load sandboxes from different locations into one TUI session:

```bash
nanosb --config ~/repos/frontend --config ~/repos/backend
```

- `--config <path>` accepts a directory (looks for `sandbox.yml` inside) or a direct file path
- CWD's `sandbox.yml` (if present) is always loaded alongside `--config` paths
- Each file's `defaults:` applies only to sandboxes within that file
- `project.path` relative paths resolve relative to their own `sandbox.yml` location
- Name collisions across files produce an error at startup

## Multi-Repo Projects

Each sandbox can point to a different repository:

```yaml
sandboxes:
  frontend-agent:
    project:
      path: ~/repos/frontend
  backend-agent:
    project:
      path: ~/repos/backend
```

If `project` is omitted entirely, no project is mounted (no auto-mount of CWD).

## Environment Variable Expansion

`${VAR}` syntax in string values expands from host environment at parse time. Missing variables produce an error with the variable name and field location.

## Error Handling

- Invalid YAML syntax -> error with parse location
- Unknown fields -> warning (not error) for forward compatibility
- Missing required `image` after merge -> error naming the sandbox
- Name collision across config files -> error naming both files
- Missing `${VAR}` -> error with variable name

## Rust Structs

```rust
pub struct SandboxFile {
    pub defaults: Option<SandboxDefaults>,
    pub sandboxes: HashMap<String, SandboxDefinition>,
}

pub struct SandboxDefaults {
    pub image: Option<String>,
    pub cpus: Option<u32>,
    pub memory: Option<u32>,
    pub timeout: Option<u32>,
    pub workdir: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub network: Option<NetworkConfigDef>,
    pub mounts: Option<Vec<MountDef>>,
    pub mcp: Option<HashMap<String, McpServerConfigDef>>,
    pub project: Option<ProjectConfigDef>,
}

pub struct SandboxDefinition {
    pub name: Option<String>,
    // ... same fields as SandboxDefaults
}
```

New module: `src/config/file.rs`. Parsing via `serde_yaml`.

## Extensibility

The config is designed to grow with the project. Future features (e.g., `skills`) add new optional fields to `SandboxDefaults` and `SandboxDefinition`. Unknown fields are warned, not rejected, so older CLI versions can read newer configs.
