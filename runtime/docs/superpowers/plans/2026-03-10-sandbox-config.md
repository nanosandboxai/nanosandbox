# Sandbox Config Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add project-level `sandbox.yml` config file support with auto-detection, multi-sandbox definitions, named sandbox aliases, and multi-config composition.

**Architecture:** New `src/config/file.rs` module for YAML parsing and merge logic. The CLI detects `sandbox.yml` in CWD, parses it into `SandboxFile` structs, merges defaults → per-sandbox → CLI overrides, and feeds the resulting `SandboxConfig` instances to the existing TUI startup path. The existing `src/config.rs` moves to `src/config/mod.rs` to keep the module organized.

**Tech Stack:** serde_yaml for YAML parsing, existing serde infrastructure, regex for `${VAR}` expansion.

---

## Chunk 1: Config Module Restructure & YAML Parsing

### Task 1: Add serde_yaml dependency

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add serde_yaml to dependencies**

Add `serde_yaml = "0.9"` to the `[dependencies]` section in `Cargo.toml`, after the existing `serde_json` line:

```toml
serde_yaml = "0.9"
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check --features cli`
Expected: compiles with no errors

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add serde_yaml dependency for sandbox config file support"
```

### Task 2: Restructure config module into directory

**Files:**
- Move: `src/config.rs` → `src/config/mod.rs`

- [ ] **Step 1: Create config directory and move file**

```bash
mkdir -p src/config
mv src/config.rs src/config/mod.rs
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check --features cli`
Expected: compiles with no errors (module path `crate::config` stays the same)

- [ ] **Step 3: Run existing tests to confirm nothing broke**

Run: `cargo test --features cli`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add src/config.rs src/config/mod.rs
git commit -m "refactor: move config.rs to config/mod.rs for module expansion"
```

### Task 3: Create SandboxFile structs and YAML deserialization

**Files:**
- Create: `src/config/file.rs`
- Modify: `src/config/mod.rs` (add `pub mod file;`)

- [ ] **Step 1: Write the failing test for YAML parsing**

Create `src/config/file.rs` with test only first:

```rust
//! Sandbox config file (sandbox.yml) parsing and merge logic.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

use super::{
    McpServerConfig, Mount, MountType, NetworkConfig, NetworkMode, NetworkScope,
    PortMapping, ProjectConfig, SandboxConfig,
};

/// Represents a parsed `sandbox.yml` file.
#[derive(Debug, Clone, Deserialize)]
pub struct SandboxFile {
    /// Default configuration inherited by all sandboxes.
    #[serde(default)]
    pub defaults: SandboxDefaults,
    /// Named sandbox definitions.
    #[serde(default)]
    pub sandboxes: HashMap<String, SandboxDefinition>,
}

/// Defaults block — all fields optional to allow partial override.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SandboxDefaults {
    pub image: Option<String>,
    pub cpus: Option<u32>,
    pub memory: Option<u32>,
    pub timeout: Option<u32>,
    pub workdir: Option<String>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    pub network: Option<NetworkDef>,
    pub mounts: Option<Vec<MountDef>>,
    pub mcp: Option<HashMap<String, McpServerConfig>>,
    pub project: Option<ProjectDef>,
}

/// Per-sandbox definition — same fields as defaults plus a name override.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SandboxDefinition {
    /// Display name override (defaults to the map key).
    pub name: Option<String>,
    pub image: Option<String>,
    pub cpus: Option<u32>,
    pub memory: Option<u32>,
    pub timeout: Option<u32>,
    pub workdir: Option<String>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    pub network: Option<NetworkDef>,
    pub mounts: Option<Vec<MountDef>>,
    pub mcp: Option<HashMap<String, McpServerConfig>>,
    pub project: Option<ProjectDef>,
}

/// Network configuration in YAML.
#[derive(Debug, Clone, Deserialize)]
pub struct NetworkDef {
    pub enabled: Option<bool>,
    pub mode: Option<NetworkMode>,
    pub scope: Option<NetworkScope>,
    pub ports: Option<Vec<String>>,
    pub dns: Option<Vec<String>>,
}

/// Mount definition in YAML.
#[derive(Debug, Clone, Deserialize)]
pub struct MountDef {
    pub host: String,
    pub container: String,
    #[serde(default)]
    pub readonly: bool,
    #[serde(default, rename = "type")]
    pub mount_type: Option<MountType>,
}

/// Project mount definition in YAML.
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectDef {
    pub path: Option<String>,
    pub branch: Option<String>,
    pub mount_point: Option<String>,
    pub auto_sync: Option<bool>,
}

/// Parse a sandbox.yml string into a SandboxFile.
pub fn parse_sandbox_file(content: &str) -> Result<SandboxFile, String> {
    serde_yaml::from_str(content).map_err(|e| format!("Failed to parse sandbox.yml: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_config() {
        let yaml = r#"
sandboxes:
  claude:
    image: nanosb-claude:latest
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        assert_eq!(file.sandboxes.len(), 1);
        assert_eq!(
            file.sandboxes["claude"].image.as_deref(),
            Some("nanosb-claude:latest")
        );
    }

    #[test]
    fn test_parse_defaults_and_sandboxes() {
        let yaml = r#"
defaults:
  image: nanosb-claude:latest
  cpus: 2
  memory: 4096

sandboxes:
  claude:
    mcp:
      github:
        command: npx
        args: ["-y", "@modelcontextprotocol/server-github"]
  codex:
    image: nanosb-codex:latest
    cpus: 4
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        assert_eq!(file.defaults.image.as_deref(), Some("nanosb-claude:latest"));
        assert_eq!(file.defaults.cpus, Some(2));
        assert_eq!(file.defaults.memory, Some(4096));
        assert_eq!(file.sandboxes.len(), 2);
        // codex overrides image
        assert_eq!(
            file.sandboxes["codex"].image.as_deref(),
            Some("nanosb-codex:latest")
        );
        assert_eq!(file.sandboxes["codex"].cpus, Some(4));
        // claude inherits image from defaults (no override)
        assert!(file.sandboxes["claude"].image.is_none());
    }

    #[test]
    fn test_parse_network_config() {
        let yaml = r#"
sandboxes:
  test:
    image: test:latest
    network:
      enabled: true
      mode: tsi
      scope: public
      ports: ["8080:80", "3000:3000/udp"]
      dns: ["8.8.8.8"]
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let net = file.sandboxes["test"].network.as_ref().unwrap();
        assert_eq!(net.enabled, Some(true));
        assert_eq!(net.ports.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_parse_project_config() {
        let yaml = r#"
sandboxes:
  agent:
    image: test:latest
    project:
      path: ~/repos/frontend
      branch: feature/work
      auto_sync: true
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let proj = file.sandboxes["agent"].project.as_ref().unwrap();
        assert_eq!(proj.path.as_deref(), Some("~/repos/frontend"));
        assert_eq!(proj.branch.as_deref(), Some("feature/work"));
        assert_eq!(proj.auto_sync, Some(true));
    }

    #[test]
    fn test_parse_name_override() {
        let yaml = r#"
sandboxes:
  claude:
    name: my-claude-sandbox
    image: test:latest
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        assert_eq!(
            file.sandboxes["claude"].name.as_deref(),
            Some("my-claude-sandbox")
        );
    }

    #[test]
    fn test_parse_invalid_yaml() {
        let yaml = "sandboxes: [this is not valid";
        let result = parse_sandbox_file(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_empty_defaults() {
        let yaml = r#"
sandboxes:
  test:
    image: alpine:latest
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        assert!(file.defaults.image.is_none());
        assert!(file.defaults.cpus.is_none());
    }
}
```

- [ ] **Step 2: Add module declaration to config/mod.rs**

Add at the top of `src/config/mod.rs`:

```rust
pub mod file;
```

- [ ] **Step 3: Run tests to verify parsing works**

Run: `cargo test --features cli config::file`
Expected: all 7 tests pass

- [ ] **Step 4: Commit**

```bash
git add src/config/file.rs src/config/mod.rs
git commit -m "feat: add sandbox.yml parsing structs and deserialization"
```

### Task 4: Environment variable expansion

**Files:**
- Modify: `src/config/file.rs`

- [ ] **Step 1: Write the failing test for env var expansion**

Add to `src/config/file.rs`:

```rust
/// Expand `${VAR}` references in a string from the host environment.
/// Returns an error if a referenced variable is not set.
pub fn expand_env_vars(input: &str) -> Result<String, String> {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut var_name = String::new();
            loop {
                match chars.next() {
                    Some('}') => break,
                    Some(ch) => var_name.push(ch),
                    None => return Err(format!("Unterminated variable reference: ${{{}", var_name)),
                }
            }
            match std::env::var(&var_name) {
                Ok(val) => result.push_str(&val),
                Err(_) => {
                    return Err(format!(
                        "Environment variable '{}' is not set",
                        var_name
                    ))
                }
            }
        } else {
            result.push(c);
        }
    }

    Ok(result)
}
```

Add tests:

```rust
    #[test]
    fn test_expand_env_vars_simple() {
        std::env::set_var("NANOSB_TEST_TOKEN", "abc123");
        let result = expand_env_vars("token=${NANOSB_TEST_TOKEN}").unwrap();
        assert_eq!(result, "token=abc123");
        std::env::remove_var("NANOSB_TEST_TOKEN");
    }

    #[test]
    fn test_expand_env_vars_missing() {
        let result = expand_env_vars("${NANOSB_NONEXISTENT_VAR_12345}");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("NANOSB_NONEXISTENT_VAR_12345"));
    }

    #[test]
    fn test_expand_env_vars_no_vars() {
        let result = expand_env_vars("plain string").unwrap();
        assert_eq!(result, "plain string");
    }

    #[test]
    fn test_expand_env_vars_multiple() {
        std::env::set_var("NANOSB_TEST_A", "hello");
        std::env::set_var("NANOSB_TEST_B", "world");
        let result = expand_env_vars("${NANOSB_TEST_A} ${NANOSB_TEST_B}").unwrap();
        assert_eq!(result, "hello world");
        std::env::remove_var("NANOSB_TEST_A");
        std::env::remove_var("NANOSB_TEST_B");
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test --features cli config::file::tests::test_expand`
Expected: all 4 tests pass

- [ ] **Step 3: Commit**

```bash
git add src/config/file.rs
git commit -m "feat: add environment variable expansion for sandbox.yml"
```

### Task 5: Merge logic — SandboxFile to Vec<SandboxConfig>

**Files:**
- Modify: `src/config/file.rs`

- [ ] **Step 1: Write port parsing helper and merge function**

Add to `src/config/file.rs` (the imports for these types are already at the top of the file from Task 3):

```rust
use std::path::Path;

/// Parse a port string like "8080:80" or "3000:3000/udp" into a PortMapping.
fn parse_port(s: &str) -> Result<PortMapping, String> {
    let (port_part, proto) = if let Some((p, pr)) = s.rsplit_once('/') {
        (p, pr.to_string())
    } else {
        (s, "tcp".to_string())
    };

    let (host, container) = port_part
        .split_once(':')
        .ok_or_else(|| format!("Invalid port mapping '{}': expected host:container", s))?;

    let host_port: u16 = host
        .parse()
        .map_err(|_| format!("Invalid host port '{}' in '{}'", host, s))?;
    let container_port: u16 = container
        .parse()
        .map_err(|_| format!("Invalid container port '{}' in '{}'", container, s))?;

    Ok(PortMapping {
        host_port,
        container_port,
        protocol: proto,
    })
}

/// Resolve a SandboxFile into a list of (sandbox_key, SandboxConfig) pairs.
///
/// `config_dir` is the directory containing the sandbox.yml file,
/// used to resolve relative `project.path` values.
pub fn resolve_sandbox_configs(
    file: &SandboxFile,
    config_dir: &Path,
) -> Result<Vec<(String, SandboxConfig)>, String> {
    let mut results = Vec::new();

    for (key, def) in &file.sandboxes {
        let defaults = &file.defaults;
        let mut config = SandboxConfig::default();

        // Name: explicit name field, or the map key
        config.name = def.name.clone().unwrap_or_else(|| key.clone());

        // Image (required after merge)
        config.image = def
            .image
            .clone()
            .or_else(|| defaults.image.clone())
            .ok_or_else(|| {
                format!(
                    "Sandbox '{}' has no image defined (not in sandbox or defaults)",
                    key
                )
            })?;

        // Scalar fields: per-sandbox > defaults > SandboxConfig::default()
        if let Some(cpus) = def.cpus.or(defaults.cpus) {
            config.cpus = cpus;
        }
        if let Some(memory) = def.memory.or(defaults.memory) {
            config.memory_mb = memory;
        }
        if let Some(timeout) = def.timeout.or(defaults.timeout) {
            config.timeout_secs = timeout;
        }
        if let Some(ref workdir) = def.workdir.as_ref().or(defaults.workdir.as_ref()) {
            config.workdir = workdir.to_string();
        }

        // Env vars: merge (defaults first, per-sandbox overrides)
        let mut env = HashMap::new();
        if let Some(ref defaults_env) = defaults.env {
            for (k, v) in defaults_env {
                env.insert(k.clone(), expand_env_vars(v)?);
            }
        }
        if let Some(ref def_env) = def.env {
            for (k, v) in def_env {
                env.insert(k.clone(), expand_env_vars(v)?);
            }
        }
        config.env = env;

        // Network: per-sandbox overrides defaults at field level
        let net_def = def.network.as_ref();
        let net_defaults = defaults.network.as_ref();
        if net_def.is_some() || net_defaults.is_some() {
            let mut net = NetworkConfig::default();
            // Apply defaults first
            if let Some(nd) = net_defaults {
                if let Some(enabled) = nd.enabled {
                    net.enabled = enabled;
                }
                if let Some(mode) = nd.mode {
                    net.mode = mode;
                }
                if let Some(scope) = nd.scope {
                    net.scope = scope;
                }
                if let Some(ref ports) = nd.ports {
                    for p in ports {
                        net.port_mappings.push(parse_port(p)?);
                    }
                }
                if let Some(ref dns) = nd.dns {
                    net.dns = dns.clone();
                }
            }
            // Apply per-sandbox overrides
            if let Some(nd) = net_def {
                if let Some(enabled) = nd.enabled {
                    net.enabled = enabled;
                }
                if let Some(mode) = nd.mode {
                    net.mode = mode;
                }
                if let Some(scope) = nd.scope {
                    net.scope = scope;
                }
                if let Some(ref ports) = nd.ports {
                    // Per-sandbox ports REPLACE defaults
                    net.port_mappings.clear();
                    for p in ports {
                        net.port_mappings.push(parse_port(p)?);
                    }
                }
                if let Some(ref dns) = nd.dns {
                    net.dns = dns.clone();
                }
            }
            config.network = net;
        }

        // Mounts: per-sandbox REPLACES defaults
        let mount_defs = def.mounts.as_ref().or(defaults.mounts.as_ref());
        if let Some(mounts) = mount_defs {
            for m in mounts {
                let host_path = if m.host.starts_with('/') || m.host.starts_with('~') {
                    let expanded = shellexpand_tilde(&m.host);
                    std::path::PathBuf::from(expanded)
                } else {
                    config_dir.join(&m.host)
                };
                config.mounts.push(Mount {
                    host_path,
                    container_path: m.container.clone(),
                    readonly: m.readonly,
                    mount_type: m.mount_type.unwrap_or(MountType::Bind),
                });
            }
        }

        // MCP servers: merge (defaults first, per-sandbox overrides)
        let mut mcp = HashMap::new();
        if let Some(ref defaults_mcp) = defaults.mcp {
            for (k, v) in defaults_mcp {
                mcp.insert(k.clone(), v.clone());
            }
        }
        if let Some(ref def_mcp) = def.mcp {
            for (k, v) in def_mcp {
                mcp.insert(k.clone(), v.clone());
            }
        }
        config.mcp_servers = mcp;

        // Project: per-sandbox overrides defaults entirely
        let proj_def = def.project.as_ref().or(defaults.project.as_ref());
        if let Some(proj) = proj_def {
            let path = if let Some(ref p) = proj.path {
                let expanded = shellexpand_tilde(p);
                if expanded.starts_with('/') {
                    std::path::PathBuf::from(expanded)
                } else {
                    config_dir.join(expanded)
                }
            } else {
                config_dir.to_path_buf()
            };

            config.project = Some(ProjectConfig {
                path,
                branch: proj.branch.clone(),
                mount_point: proj
                    .mount_point
                    .clone()
                    .unwrap_or_else(|| "/workspace".to_string()),
                auto_sync: proj.auto_sync.unwrap_or(false),
            });
        }

        results.push((key.clone(), config));
    }

    Ok(results)
}

/// Expand `~` at the start of a path to the user's home directory.
fn shellexpand_tilde(path: &str) -> String {
    if path.starts_with("~/") || path == "~" {
        if let Some(home) = dirs::home_dir() {
            return path.replacen('~', &home.to_string_lossy(), 1);
        }
    }
    path.to_string()
}
```

- [ ] **Step 2: Write tests for merge logic**

Add tests:

```rust
    #[test]
    fn test_resolve_minimal() {
        let yaml = r#"
sandboxes:
  test:
    image: alpine:latest
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs =
            resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].0, "test");
        assert_eq!(configs[0].1.image, "alpine:latest");
        assert_eq!(configs[0].1.name, "test"); // key becomes name
    }

    #[test]
    fn test_resolve_defaults_merge() {
        let yaml = r#"
defaults:
  image: default:latest
  cpus: 2
  memory: 4096

sandboxes:
  a:
    cpus: 8
  b: {}
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs =
            resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        let a = configs.iter().find(|(k, _)| k == "a").unwrap();
        let b = configs.iter().find(|(k, _)| k == "b").unwrap();
        assert_eq!(a.1.cpus, 8); // override
        assert_eq!(a.1.memory_mb, 4096); // inherited
        assert_eq!(a.1.image, "default:latest"); // inherited
        assert_eq!(b.1.cpus, 2); // inherited
    }

    #[test]
    fn test_resolve_missing_image_errors() {
        let yaml = r#"
sandboxes:
  no_image: {}
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let result =
            resolve_sandbox_configs(&file, std::path::Path::new("/tmp"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no_image"));
    }

    #[test]
    fn test_resolve_env_merge() {
        std::env::set_var("NANOSB_TEST_RESOLVE", "resolved");
        let yaml = r#"
defaults:
  env:
    SHARED: shared_value
    OVERRIDE: default_val

sandboxes:
  test:
    image: test:latest
    env:
      OVERRIDE: sandbox_val
      EXTRA: "${NANOSB_TEST_RESOLVE}"
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs =
            resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        let env = &configs[0].1.env;
        assert_eq!(env["SHARED"], "shared_value");
        assert_eq!(env["OVERRIDE"], "sandbox_val"); // sandbox wins
        assert_eq!(env["EXTRA"], "resolved"); // env var expanded
        std::env::remove_var("NANOSB_TEST_RESOLVE");
    }

    #[test]
    fn test_resolve_name_override() {
        let yaml = r#"
sandboxes:
  claude:
    name: my-claude
    image: test:latest
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs =
            resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        assert_eq!(configs[0].1.name, "my-claude");
    }

    #[test]
    fn test_resolve_mcp_merge() {
        let yaml = r#"
defaults:
  mcp:
    github:
      command: npx
      args: ["-y", "@modelcontextprotocol/server-github"]
    filesystem:
      command: npx
      args: ["-y", "@modelcontextprotocol/server-filesystem"]

sandboxes:
  test:
    image: test:latest
    mcp:
      github:
        command: uvx
        args: ["different-github"]
      memory:
        command: npx
        args: ["-y", "@modelcontextprotocol/server-memory"]
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs =
            resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        let mcp = &configs[0].1.mcp_servers;
        assert_eq!(mcp.len(), 3); // github (overridden) + filesystem (inherited) + memory (new)
        assert_eq!(mcp["github"].command, "uvx"); // per-sandbox wins
        assert_eq!(mcp["filesystem"].command, "npx"); // inherited
        assert!(mcp.contains_key("memory"));
    }

    #[test]
    fn test_parse_port_tcp() {
        let pm = parse_port("8080:80").unwrap();
        assert_eq!(pm.host_port, 8080);
        assert_eq!(pm.container_port, 80);
        assert_eq!(pm.protocol, "tcp");
    }

    #[test]
    fn test_parse_port_udp() {
        let pm = parse_port("3000:3000/udp").unwrap();
        assert_eq!(pm.host_port, 3000);
        assert_eq!(pm.container_port, 3000);
        assert_eq!(pm.protocol, "udp");
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test --features cli config::file`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add src/config/file.rs
git commit -m "feat: add sandbox config merge logic (defaults + per-sandbox + env expansion)"
```

## Chunk 2: Config File Detection & CLI Integration

### Task 6: Add config file detection and loading function

**Files:**
- Modify: `src/config/file.rs`

- [ ] **Step 1: Add load_sandbox_files function**

Add to `src/config/file.rs`:

```rust
/// Load and validate sandbox config files.
///
/// Searches for `sandbox.yml` or `sandbox.yaml` in the given directory.
/// Returns `None` if no config file is found.
pub fn find_sandbox_file(dir: &Path) -> Option<std::path::PathBuf> {
    let yml = dir.join("sandbox.yml");
    if yml.exists() {
        return Some(yml);
    }
    let yaml = dir.join("sandbox.yaml");
    if yaml.exists() {
        return Some(yaml);
    }
    None
}

/// Load a sandbox config file, parse it, expand env vars, and resolve
/// into a list of (sandbox_key, SandboxConfig) pairs.
pub fn load_sandbox_file(path: &Path) -> Result<Vec<(String, SandboxConfig)>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;

    let file = parse_sandbox_file(&content)?;

    let config_dir = path.parent().unwrap_or(Path::new("."));
    resolve_sandbox_configs(&file, config_dir)
}

/// Load sandbox configs from multiple paths (directories or files).
/// Each path is resolved independently. Returns all sandbox configs
/// with collision detection on names.
pub fn load_sandbox_files(
    paths: &[std::path::PathBuf],
) -> Result<Vec<(String, SandboxConfig)>, String> {
    let mut all_configs = Vec::new();
    let mut seen_names: HashMap<String, String> = HashMap::new(); // name -> source file

    for path in paths {
        let file_path = if path.is_dir() {
            match find_sandbox_file(path) {
                Some(f) => f,
                None => {
                    return Err(format!(
                        "No sandbox.yml found in {}",
                        path.display()
                    ))
                }
            }
        } else {
            path.clone()
        };

        let configs = load_sandbox_file(&file_path)?;
        let source = file_path.display().to_string();

        for (key, config) in configs {
            let name = &config.name;
            if let Some(prev_source) = seen_names.get(name) {
                return Err(format!(
                    "Sandbox name '{}' defined in both {} and {}",
                    name, prev_source, source
                ));
            }
            seen_names.insert(name.clone(), source.clone());
            all_configs.push((key, config));
        }
    }

    Ok(all_configs)
}
```

- [ ] **Step 2: Write tests**

```rust
    #[test]
    fn test_find_sandbox_file_yml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sandbox.yml"), "sandboxes: {}").unwrap();
        let found = find_sandbox_file(dir.path());
        assert!(found.is_some());
        assert!(found.unwrap().ends_with("sandbox.yml"));
    }

    #[test]
    fn test_find_sandbox_file_yaml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sandbox.yaml"), "sandboxes: {}").unwrap();
        let found = find_sandbox_file(dir.path());
        assert!(found.is_some());
        assert!(found.unwrap().ends_with("sandbox.yaml"));
    }

    #[test]
    fn test_find_sandbox_file_none() {
        let dir = tempfile::tempdir().unwrap();
        let found = find_sandbox_file(dir.path());
        assert!(found.is_none());
    }

    #[test]
    fn test_load_sandbox_file_integration() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
defaults:
  cpus: 2

sandboxes:
  test:
    image: alpine:latest
"#;
        let path = dir.path().join("sandbox.yml");
        std::fs::write(&path, yaml).unwrap();
        let configs = load_sandbox_file(&path).unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].1.cpus, 2);
    }

    #[test]
    fn test_load_sandbox_files_name_collision() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        let yaml = r#"
sandboxes:
  claude:
    image: test:latest
"#;
        std::fs::write(dir1.path().join("sandbox.yml"), yaml).unwrap();
        std::fs::write(dir2.path().join("sandbox.yml"), yaml).unwrap();

        let result = load_sandbox_files(&[
            dir1.path().to_path_buf(),
            dir2.path().to_path_buf(),
        ]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("claude"));
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test --features cli config::file`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add src/config/file.rs
git commit -m "feat: add sandbox config file detection and multi-file loading"
```

### Task 7: Add re-exports for new config file module

**Files:**
- Modify: `src/config/mod.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Add re-exports**

In `src/config/mod.rs`, ensure `pub mod file;` is present.

In `src/lib.rs`, add to the re-exports:

```rust
pub use config::file::{find_sandbox_file, load_sandbox_file, load_sandbox_files, SandboxFile};
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check --features cli`
Expected: compiles

- [ ] **Step 3: Commit**

```bash
git add src/config/mod.rs src/lib.rs
git commit -m "feat: re-export sandbox config file types and functions"
```

### Task 8: Add --config and --sandbox CLI flags

**Files:**
- Modify: `src/bin/nanosb.rs`

- [ ] **Step 1: Add CLI flags to Cli struct**

Add to the `Cli` struct in `src/bin/nanosb.rs`:

```rust
    /// Path to sandbox.yml config file or directory containing one.
    /// Can be specified multiple times to load from multiple configs.
    #[arg(long = "config", global = true)]
    pub configs: Vec<String>,

    /// Start only the named sandbox from the config file (instead of all).
    #[arg(long, global = true)]
    pub sandbox: Option<String>,
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check --features cli`
Expected: compiles

- [ ] **Step 3: Commit**

```bash
git add src/bin/nanosb.rs
git commit -m "feat: add --config and --sandbox CLI flags"
```

### Task 9: Wire config file loading into TUI startup

**Files:**
- Modify: `src/bin/nanosb.rs` (the `None` arm in `run()`)
- Modify: `src/tui/run.rs` (add `sandbox_configs` parameter to `run_tui`)

- [ ] **Step 1: Update run_tui signature to accept sandbox configs**

In `src/tui/run.rs`, change `run_tui` signature:

```rust
pub async fn run_tui(
    project_path: Option<std::path::PathBuf>,
    sandbox_configs: Vec<(String, SandboxConfig)>,
) -> anyhow::Result<()> {
```

After creating the App and event channel (after `spawn_terminal_event_reader`), add auto-start logic:

```rust
    // Auto-start sandboxes from config file.
    for (key, config) in &sandbox_configs {
        add_agent_from_config(
            &mut app,
            key,
            config.clone(),
            &tx,
        );
    }
```

- [ ] **Step 2: Add add_agent_from_config function**

Add to `src/tui/run.rs`:

```rust
/// Add an agent panel from a resolved SandboxConfig (from sandbox.yml).
fn add_agent_from_config(
    app: &mut App,
    key: &str,
    config: SandboxConfig,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    let agent_name = &config.name;
    let image_name = &config.image;

    let mut panel = AgentPanel::new(agent_name);
    panel.chat_history.push(ChatMessage {
        role: MessageRole::System,
        content: format!("Launching {} (image: {})...", agent_name, image_name),
    });

    // Copy env vars from config to panel.
    for (k, v) in &config.env {
        panel.env.insert(k.clone(), v.clone());
    }

    // Auto-detect API keys from host environment (if not already in config env).
    for (api_key, _) in &required_api_keys(key) {
        if !panel.env.contains_key(*api_key) {
            if let Ok(val) = std::env::var(api_key) {
                panel.env.insert(api_key.to_string(), val);
            }
        }
    }

    app.panels.push(panel);
    let panel_idx = app.panels.len() - 1;
    app.focused_panel = panel_idx;
    app.show_welcome = false;
    app.focus_panel_input();

    let tx = tx.clone();
    tokio::spawn(async move {
        match Sandbox::create(config).await {
            Ok(mut sandbox) => {
                let short_id = sandbox.id()[..8.min(sandbox.id().len())].to_string();

                match sandbox.start().await {
                    Ok(()) => {
                        let project_mount = sandbox.take_project_mount();
                        let sb = Arc::new(Mutex::new(sandbox));
                        let _ = tx.send(AppEvent::SandboxReady {
                            panel_idx,
                            sandbox: sb,
                            short_id,
                            project_mount,
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(AppEvent::SandboxFailed {
                            panel_idx,
                            error: format!("Failed to start sandbox: {}", e),
                        });
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(AppEvent::SandboxFailed {
                    panel_idx,
                    error: format!("Failed to create sandbox: {}", e),
                });
            }
        }
    });
}
```

- [ ] **Step 3: Update CLI run() to load config and pass to run_tui**

In `src/bin/nanosb.rs`, update the `None` arm (TUI mode):

```rust
            None => {
                // Collect config file paths.
                let mut config_paths: Vec<std::path::PathBuf> = cli
                    .configs
                    .iter()
                    .map(std::path::PathBuf::from)
                    .collect();

                // Auto-detect sandbox.yml in CWD.
                let cwd = std::env::current_dir()?;
                if nanosandbox::find_sandbox_file(&cwd).is_some()
                    && !config_paths.contains(&cwd)
                {
                    config_paths.insert(0, cwd.clone());
                }

                // Load and resolve sandbox configs.
                let mut sandbox_configs = if config_paths.is_empty() {
                    Vec::new()
                } else {
                    nanosandbox::load_sandbox_files(&config_paths)
                        .map_err(|e| anyhow::anyhow!("{}", e))?
                };

                // Filter to a single sandbox if --sandbox is specified.
                if let Some(ref name) = cli.sandbox {
                    sandbox_configs.retain(|(key, config)| {
                        key == name || config.name == *name
                    });
                    if sandbox_configs.is_empty() {
                        anyhow::bail!(
                            "Sandbox '{}' not found in config files",
                            name
                        );
                    }
                }

                // Project path for sandboxes without explicit project config.
                let project_path = cli.project
                    .map(std::path::PathBuf::from)
                    .or_else(|| {
                        let cwd = std::env::current_dir().ok()?;
                        if cwd.join(".git").exists() {
                            Some(cwd)
                        } else {
                            None
                        }
                    });

                nanosandbox::tui::run::run_tui(project_path, sandbox_configs).await
            }
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check --features cli`
Expected: compiles

- [ ] **Step 5: Commit**

```bash
git add src/bin/nanosb.rs src/tui/run.rs
git commit -m "feat: wire sandbox.yml loading into TUI startup with auto-detection"
```

## Chunk 3: Name Parameter & CLI Tests

### Task 10: Add --name flag to TUI /add command

**Files:**
- Modify: `src/tui/commands.rs`
- Modify: `src/tui/run.rs`

- [ ] **Step 1: Add name field to Command::AddAgent**

In `src/tui/commands.rs`, add `name` to the `AddAgent` variant:

```rust
    AddAgent {
        agent: String,
        image: Option<String>,
        project: Option<String>,
        branch: Option<String>,
        name: Option<String>,
    },
```

- [ ] **Step 2: Add --name parsing to parse_add()**

In `src/tui/commands.rs`, add `let mut name = None;` after `let mut branch = None;` (line 181), then add a new arm in the `while i < parts.len()` match:

```rust
            "--name" => {
                match parts.get(i + 1) {
                    Some(v) => { name = Some(v.to_string()); i += 2; }
                    None => return ParseResult::Err(
                        "--name requires a value\n\
                         Usage: /add <agent> --name <name>".to_string(),
                    ),
                }
            }
```

Update the `ParseResult::Ok` return at the end of `parse_add` to include `name`:

```rust
    ParseResult::Ok(Command::AddAgent {
        agent: agent.to_string(),
        image,
        project,
        branch,
        name,
    })
```

Update the usage string in the error messages to include `--name`.

- [ ] **Step 3: Thread name through add_agent in run.rs**

In `src/tui/run.rs`, update the `Command::AddAgent` match arm to destructure `name`:

```rust
        Command::AddAgent { agent, image, project, branch, name } => {
            add_agent(app, &agent, image.as_deref(), project.as_deref(), branch.as_deref(), name.as_deref(), tx);
        }
```

Update `add_agent` function signature to accept `name: Option<&str>` and use it when building SandboxConfig:

```rust
fn add_agent(
    app: &mut App,
    agent: &str,
    image: Option<&str>,
    project: Option<&str>,
    branch: Option<&str>,
    name: Option<&str>,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
```

Replace the sandbox name construction:

```rust
    let sandbox_name = name
        .map(String::from)
        .unwrap_or_else(|| format!("tui-{}", agent));
```

And use `sandbox_name` in the builder: `.name(&sandbox_name)`.

- [ ] **Step 4: Verify it compiles**

Run: `cargo check --features cli`
Expected: compiles

- [ ] **Step 5: Commit**

```bash
git add src/tui/commands.rs src/tui/run.rs
git commit -m "feat: add --name flag to /add TUI command"
```

### Task 11: Add NAME column to nanosb ps output

**Files:**
- Modify: `src/bin/nanosb.rs`

- [ ] **Step 1: Add NAME field to SandboxRow struct**

In `src/bin/nanosb.rs`, update the `SandboxRow` struct (around line 157) to add a `name` field after `id`:

```rust
    #[derive(Tabled)]
    struct SandboxRow {
        #[tabled(rename = "ID")]
        id: String,
        #[tabled(rename = "NAME")]
        name: String,
        #[tabled(rename = "IMAGE")]
        image: String,
        #[tabled(rename = "STATUS")]
        status: String,
        #[tabled(rename = "CREATED")]
        created: String,
    }
```

- [ ] **Step 2: Add name to SandboxRow construction in cmd_ps**

In the `cmd_ps` function (around line 645), update the `.map(|s| { ... })` closure to include `name`:

```rust
                    SandboxRow {
                        id: s.id[..12].to_string(),
                        name: s.name.clone(),
                        image: s.image.clone(),
                        status: status_str,
                        created: format_duration(duration),
                    }
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check --features cli`
Expected: compiles

- [ ] **Step 4: Commit**

```bash
git add src/bin/nanosb.rs
git commit -m "feat: show NAME column in nanosb ps output"
```

### Task 12: CLI flag overrides for sandbox configs

**Files:**
- Modify: `src/bin/nanosb.rs`
- Modify: `src/config/file.rs`

- [ ] **Step 1: Add global override CLI flags**

In `src/bin/nanosb.rs`, add optional global override flags to the `Cli` struct:

```rust
    /// Override CPU cores for all sandboxes from config
    #[arg(long, global = true)]
    pub cpus: Option<u32>,

    /// Override memory (MB) for all sandboxes from config
    #[arg(long, global = true)]
    pub memory: Option<u32>,

    /// Override timeout (seconds) for all sandboxes from config
    #[arg(long, global = true)]
    pub timeout: Option<u32>,
```

- [ ] **Step 2: Add apply_cli_overrides function**

Add to `src/config/file.rs`:

```rust
/// Apply CLI flag overrides to a list of resolved sandbox configs.
/// CLI flags are the final layer of the merge order:
///   SandboxConfig::default() → defaults block → per-sandbox → CLI flags
pub fn apply_cli_overrides(
    configs: &mut [(String, SandboxConfig)],
    cpus: Option<u32>,
    memory: Option<u32>,
    timeout: Option<u32>,
) {
    for (_, config) in configs.iter_mut() {
        if let Some(cpus) = cpus {
            config.cpus = cpus;
        }
        if let Some(memory) = memory {
            config.memory_mb = memory;
        }
        if let Some(timeout) = timeout {
            config.timeout_secs = timeout;
        }
    }
}
```

- [ ] **Step 3: Wire overrides into CLI run()**

In `src/bin/nanosb.rs`, in the `None` arm (TUI mode), after loading sandbox configs and before passing to `run_tui`, add:

```rust
                // Apply CLI flag overrides (merge step 4).
                nanosandbox::config::file::apply_cli_overrides(
                    &mut sandbox_configs,
                    cli.cpus,
                    cli.memory,
                    cli.timeout,
                );
```

- [ ] **Step 4: Write test**

Add to `src/config/file.rs` tests:

```rust
    #[test]
    fn test_apply_cli_overrides() {
        let yaml = r#"
sandboxes:
  test:
    image: alpine:latest
    cpus: 2
    memory: 4096
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let mut configs =
            resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        apply_cli_overrides(&mut configs, Some(8), None, Some(1200));
        assert_eq!(configs[0].1.cpus, 8); // overridden
        assert_eq!(configs[0].1.memory_mb, 4096); // not overridden
        assert_eq!(configs[0].1.timeout_secs, 1200); // overridden
    }
```

- [ ] **Step 5: Run tests**

Run: `cargo test --features cli config::file::tests::test_apply_cli`
Expected: test passes

- [ ] **Step 6: Verify full build**

Run: `cargo check --features cli`
Expected: compiles

- [ ] **Step 7: Commit**

```bash
git add src/bin/nanosb.rs src/config/file.rs
git commit -m "feat: add CLI flag overrides for sandbox config (--cpus, --memory, --timeout)"
```

### Task 13: Name validation helper

**Files:**
- Modify: `src/config/file.rs`

- [ ] **Step 1: Write failing test for name validation**

```rust
/// Validate a sandbox name: lowercase alphanumeric + hyphens, max 64 chars.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Sandbox name cannot be empty".to_string());
    }
    if name.len() > 64 {
        return Err(format!(
            "Sandbox name '{}' exceeds 64 character limit ({} chars)",
            name,
            name.len()
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(format!(
            "Sandbox name '{}' contains invalid characters (allowed: lowercase a-z, 0-9, hyphens)",
            name
        ));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(format!(
            "Sandbox name '{}' cannot start or end with a hyphen",
            name
        ));
    }
    Ok(())
}
```

Tests:

```rust
    #[test]
    fn test_validate_name_valid() {
        assert!(validate_name("claude").is_ok());
        assert!(validate_name("my-sandbox-1").is_ok());
        assert!(validate_name("a").is_ok());
    }

    #[test]
    fn test_validate_name_invalid() {
        assert!(validate_name("").is_err());
        assert!(validate_name("HAS_UPPER").is_err());
        assert!(validate_name("has spaces").is_err());
        assert!(validate_name("-starts-with-hyphen").is_err());
        assert!(validate_name("ends-with-hyphen-").is_err());
        assert!(validate_name(&"a".repeat(65)).is_err());
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test --features cli config::file::tests::test_validate`
Expected: tests pass

- [ ] **Step 3: Integrate validation into resolve_sandbox_configs**

Add a call to `validate_name` in `resolve_sandbox_configs` after setting `config.name`.

- [ ] **Step 4: Commit**

```bash
git add src/config/file.rs
git commit -m "feat: add sandbox name validation (lowercase alphanumeric + hyphens, max 64)"
```

### Task 14: CLI integration tests

**Files:**
- Create: `tests/sandbox_config.rs`

- [ ] **Step 1: Write integration tests**

```rust
//! Integration tests for sandbox.yml config file support.

use nanosandbox::config::file::{
    expand_env_vars, find_sandbox_file, load_sandbox_file, parse_sandbox_file,
    resolve_sandbox_configs, validate_name,
};

#[test]
fn test_full_config_roundtrip() {
    let yaml = r#"
defaults:
  image: nanosb-claude:latest
  cpus: 2
  memory: 4096
  workdir: /workspace
  timeout: 600
  network:
    enabled: true
    mode: tsi
    scope: any
  mcp:
    github:
      command: npx
      args: ["-y", "@modelcontextprotocol/server-github"]

sandboxes:
  claude:
    name: claude-dev
    mcp:
      filesystem:
        command: npx
        args: ["-y", "@modelcontextprotocol/server-filesystem"]
  codex:
    image: nanosb-codex:latest
    cpus: 4
    mcp:
      github:
        command: uvx
        args: ["custom-github-server"]
"#;

    let file = parse_sandbox_file(yaml).unwrap();
    let configs =
        resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();

    assert_eq!(configs.len(), 2);

    let claude = configs.iter().find(|(k, _)| k == "claude").unwrap();
    assert_eq!(claude.1.name, "claude-dev");
    assert_eq!(claude.1.image, "nanosb-claude:latest"); // inherited
    assert_eq!(claude.1.cpus, 2); // inherited
    assert_eq!(claude.1.mcp_servers.len(), 2); // github inherited + filesystem
    assert_eq!(claude.1.mcp_servers["github"].command, "npx"); // inherited

    let codex = configs.iter().find(|(k, _)| k == "codex").unwrap();
    assert_eq!(codex.1.name, "codex"); // key as name
    assert_eq!(codex.1.image, "nanosb-codex:latest"); // overridden
    assert_eq!(codex.1.cpus, 4); // overridden
    assert_eq!(codex.1.memory_mb, 4096); // inherited
    assert_eq!(codex.1.mcp_servers["github"].command, "uvx"); // overridden
}

#[test]
fn test_multi_repo_config() {
    let yaml = r#"
defaults:
  image: agent:latest

sandboxes:
  frontend:
    project:
      path: /home/user/repos/frontend
      branch: nanosb/feature
  backend:
    project:
      path: /home/user/repos/backend
"#;

    let file = parse_sandbox_file(yaml).unwrap();
    let configs =
        resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();

    let frontend = configs.iter().find(|(k, _)| k == "frontend").unwrap();
    let backend = configs.iter().find(|(k, _)| k == "backend").unwrap();

    assert_eq!(
        frontend.1.project.as_ref().unwrap().path.to_str().unwrap(),
        "/home/user/repos/frontend"
    );
    assert_eq!(
        backend.1.project.as_ref().unwrap().path.to_str().unwrap(),
        "/home/user/repos/backend"
    );
}
```

- [ ] **Step 2: Run integration tests**

Run: `cargo test --features cli --test sandbox_config`
Expected: tests pass

- [ ] **Step 3: Commit**

```bash
git add tests/sandbox_config.rs
git commit -m "test: add integration tests for sandbox.yml config support"
```

### Task 15: Update help text and documentation

**Files:**
- Modify: `src/tui/run.rs` (help command output)
- Modify: `src/bin/nanosb.rs` (CLI help text)

- [ ] **Step 1: Update /help output to mention sandbox.yml**

In the `Command::Help` handler in `src/tui/run.rs`, add a line:

```
"  Config: Place sandbox.yml in project root for auto-start\n",
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check --features cli`
Expected: compiles

- [ ] **Step 3: Commit**

```bash
git add src/tui/run.rs src/bin/nanosb.rs
git commit -m "docs: update help text to mention sandbox.yml config support"
```
