//! Sandbox config file (sandbox.yml) parsing and merge logic.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

use runtime::{
    Mount, MountType, NetworkConfig, NetworkMode, NetworkScope, PortMapping, ProjectConfig,
    RootfsMode, SandboxConfig,
};

use super::{AgentSandboxConfig, McpServerConfig};

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
    /// Path to a .env file to load environment variables from.
    pub env_file: Option<String>,
    pub network: Option<NetworkDef>,
    pub mounts: Option<Vec<MountDef>>,
    pub mcp: Option<HashMap<String, McpServerConfig>>,
    pub project: Option<ProjectDef>,
    /// Agent definition name from registry.
    pub agent: Option<String>,
    /// Skill names from registry.
    pub skills: Option<Vec<String>>,
    /// Enable auto/headless mode.
    pub auto_mode: Option<bool>,
    /// Agent permission level.
    pub permissions: Option<super::Permissions>,
    /// Task prompt for headless mode.
    pub prompt: Option<String>,
    /// Model identifier (e.g., "claude-sonnet-4-5-20250929"). Inherited by sandboxes.
    pub model: Option<String>,
    /// Root filesystem mode for Windows VMs: "vhdx" or "plan9". Ignored on macOS/Linux.
    pub rootfs_mode: Option<String>,
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
    /// Path to a .env file to load environment variables from.
    pub env_file: Option<String>,
    pub network: Option<NetworkDef>,
    pub mounts: Option<Vec<MountDef>>,
    pub mcp: Option<HashMap<String, McpServerConfig>>,
    pub project: Option<ProjectDef>,
    /// Agent definition name from registry.
    pub agent: Option<String>,
    /// Skill names from registry.
    pub skills: Option<Vec<String>>,
    /// Enable auto/headless mode.
    pub auto_mode: Option<bool>,
    /// Agent permission level.
    pub permissions: Option<super::Permissions>,
    /// Task prompt for headless mode.
    pub prompt: Option<String>,
    /// Agent type (determines CLI command and config format). Per-sandbox only.
    #[serde(rename = "type")]
    pub agent_type: Option<String>,
    /// Model identifier (e.g., "claude-sonnet-4-5-20250929").
    pub model: Option<String>,
    /// Root filesystem mode for Windows VMs: "vhdx" or "plan9". Ignored on macOS/Linux.
    pub rootfs_mode: Option<String>,
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
                    None => {
                        return Err(format!("Unterminated variable reference: ${{{}", var_name))
                    }
                }
            }
            match std::env::var(&var_name) {
                Ok(val) => result.push_str(&val),
                Err(_) => return Err(format!("Environment variable '{}' is not set", var_name)),
            }
        } else {
            result.push(c);
        }
    }

    Ok(result)
}

/// Load environment variables from a .env file.
///
/// Lines are parsed as KEY=VALUE. Empty lines and lines starting with `#` are
/// skipped. The path is resolved relative to `config_dir` if not absolute.
pub fn load_env_file(path: &str, config_dir: &Path) -> Result<HashMap<String, String>, String> {
    let resolved = if path.starts_with('/') || path.starts_with('~') {
        let expanded = shellexpand_tilde(path);
        std::path::PathBuf::from(expanded)
    } else {
        config_dir.join(path)
    };

    let content = std::fs::read_to_string(&resolved)
        .map_err(|e| format!("Failed to read env_file '{}': {}", resolved.display(), e))?;

    let mut vars = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            vars.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    Ok(vars)
}

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

/// Resolve a SandboxFile into a list of (sandbox_key, AgentSandboxConfig) pairs.
///
/// `config_dir` is the directory containing the sandbox.yml file,
/// used to resolve relative `project.path` values.
pub fn resolve_sandbox_configs(
    file: &SandboxFile,
    config_dir: &Path,
) -> Result<Vec<(String, AgentSandboxConfig)>, String> {
    let mut results = Vec::new();

    for (key, def) in &file.sandboxes {
        let defaults = &file.defaults;
        let name = def.name.clone().unwrap_or_else(|| key.clone());
        validate_name(&name)?;
        let mut config = SandboxConfig {
            name,
            ..SandboxConfig::default()
        };
        let mut agent_config = AgentSandboxConfig::default();

        // Image (required after merge). Bare names like "claude" are normalized
        // to the agents registry (ghcr.io/runtimeai/agents-registry/claude:latest).
        config.image = def
            .image
            .clone()
            .or_else(|| defaults.image.clone())
            .map(|img| super::normalize_image(&img))
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

        // Env vars merge order: defaults env_file → defaults env → per-sandbox env_file → per-sandbox env
        let mut env = HashMap::new();
        if let Some(ref path) = defaults.env_file {
            let file_vars = load_env_file(path, config_dir)?;
            env.extend(file_vars);
        }
        if let Some(ref defaults_env) = defaults.env {
            for (k, v) in defaults_env {
                env.insert(k.clone(), expand_env_vars(v)?);
            }
        }
        if let Some(ref path) = def.env_file {
            let file_vars = load_env_file(path, config_dir)?;
            env.extend(file_vars);
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
        agent_config.mcp_servers = mcp;

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

        // Agent: per-sandbox overrides defaults
        agent_config.agent = def.agent.clone().or_else(|| defaults.agent.clone());

        // Skills: per-sandbox replaces defaults (not merge)
        agent_config.skills = def
            .skills
            .clone()
            .or_else(|| defaults.skills.clone())
            .unwrap_or_default();

        // Auto mode: per-sandbox overrides defaults
        agent_config.auto_mode = def.auto_mode.or(defaults.auto_mode).unwrap_or(false);

        // Permissions: per-sandbox overrides defaults
        agent_config.permissions = def
            .permissions
            .or(defaults.permissions)
            .unwrap_or(super::Permissions::Default);

        // Prompt: per-sandbox overrides defaults
        agent_config.prompt = def.prompt.clone().or_else(|| defaults.prompt.clone());

        // Validate: prompt is required when auto_mode is enabled.
        if agent_config.auto_mode && agent_config.prompt.is_none() {
            return Err(format!(
                "Sandbox '{}': 'prompt' is required when 'auto_mode' is true",
                key,
            ));
        }

        // Agent type: per-sandbox only (NOT inherited from defaults).
        if let Some(ref type_str) = def.agent_type {
            agent_config.agent_type = Some(
                type_str
                    .parse::<super::AgentType>()
                    .map_err(|e| format!("Sandbox '{}': {}", key, e))?,
            );
        }

        // Rootfs mode (Windows only): per-sandbox overrides defaults.
        if let Some(ref mode_str) = def.rootfs_mode.as_ref().or(defaults.rootfs_mode.as_ref()) {
            config.rootfs_mode = mode_str
                .parse::<RootfsMode>()
                .map_err(|e| format!("Sandbox '{}': {}", key, e))?;
        }

        // Model: per-sandbox overrides defaults.
        agent_config.model = def.model.clone().or_else(|| defaults.model.clone());

        // Validate model against known models if both type and model are set.
        if let (Some(agent_type), Some(ref model)) = (agent_config.agent_type, &agent_config.model)
        {
            if let Some(registry) = super::models::ModelsRegistry::load() {
                registry
                    .validate(agent_type.as_str(), model)
                    .map_err(|e| format!("Sandbox '{}': {}", key, e))?;
            }
        }

        agent_config.runtime = config;
        results.push((key.clone(), agent_config));
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
/// into a list of (sandbox_key, AgentSandboxConfig) pairs.
pub fn load_sandbox_file(path: &Path) -> Result<Vec<(String, AgentSandboxConfig)>, String> {
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
) -> Result<Vec<(String, AgentSandboxConfig)>, String> {
    let mut all_configs = Vec::new();
    let mut seen_names: HashMap<String, String> = HashMap::new();

    for path in paths {
        let file_path = if path.is_dir() {
            match find_sandbox_file(path) {
                Some(f) => f,
                None => return Err(format!("No sandbox.yml found in {}", path.display())),
            }
        } else {
            path.clone()
        };

        let configs = load_sandbox_file(&file_path)?;
        let source = file_path.display().to_string();

        for (key, config) in configs {
            let name = &config.runtime.name;
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

/// Apply CLI flag overrides to a list of resolved sandbox configs.
/// CLI flags are the final layer of the merge order:
///   SandboxConfig::default() → defaults block → per-sandbox → CLI flags
pub fn apply_cli_overrides(
    configs: &mut [(String, AgentSandboxConfig)],
    cpus: Option<u32>,
    memory: Option<u32>,
    timeout: Option<u32>,
    permissions: Option<super::Permissions>,
    cli_env: &[(String, String)],
    rootfs_mode: Option<RootfsMode>,
) {
    for (_, config) in configs.iter_mut() {
        if let Some(cpus) = cpus {
            config.runtime.cpus = cpus;
        }
        if let Some(memory) = memory {
            config.runtime.memory_mb = memory;
        }
        if let Some(timeout) = timeout {
            config.runtime.timeout_secs = timeout;
        }
        if let Some(perm) = permissions {
            config.permissions = perm;
        }
        if let Some(mode) = rootfs_mode {
            config.runtime.rootfs_mode = mode;
        }
        // CLI --env / --env-file override all other env sources.
        for (k, v) in cli_env {
            config.runtime.env.insert(k.clone(), v.clone());
        }
    }
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
        assert_eq!(
            file.sandboxes["codex"].image.as_deref(),
            Some("nanosb-codex:latest")
        );
        assert_eq!(file.sandboxes["codex"].cpus, Some(4));
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

    #[test]
    fn test_resolve_minimal() {
        let yaml = r#"
sandboxes:
  test:
    image: alpine:latest
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].0, "test");
        // Bare names get normalized to agents registry
        assert_eq!(
            configs[0].1.runtime.image,
            "ghcr.io/runtimeai/agents-registry/alpine:latest"
        );
        assert_eq!(configs[0].1.runtime.name, "test");
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
        let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        let a = configs.iter().find(|(k, _)| k == "a").unwrap();
        let b = configs.iter().find(|(k, _)| k == "b").unwrap();
        assert_eq!(a.1.runtime.cpus, 8);
        assert_eq!(a.1.runtime.memory_mb, 4096);
        assert_eq!(
            a.1.runtime.image,
            "ghcr.io/runtimeai/agents-registry/default:latest"
        );
        assert_eq!(b.1.runtime.cpus, 2);
    }

    #[test]
    fn test_resolve_missing_image_errors() {
        let yaml = r#"
sandboxes:
  no_image: {}
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let result = resolve_sandbox_configs(&file, std::path::Path::new("/tmp"));
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
        let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        let env = &configs[0].1.runtime.env;
        assert_eq!(env["SHARED"], "shared_value");
        assert_eq!(env["OVERRIDE"], "sandbox_val");
        assert_eq!(env["EXTRA"], "resolved");
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
        let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        assert_eq!(configs[0].1.runtime.name, "my-claude");
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
        let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        let mcp = &configs[0].1.mcp_servers;
        assert_eq!(mcp.len(), 3);
        assert_eq!(mcp["github"].command, "uvx");
        assert_eq!(mcp["filesystem"].command, "npx");
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
        assert_eq!(configs[0].1.runtime.cpus, 2);
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

        let result = load_sandbox_files(&[dir1.path().to_path_buf(), dir2.path().to_path_buf()]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("claude"));
    }

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
        let mut configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        apply_cli_overrides(&mut configs, Some(8), None, Some(1200), None, &[], None);
        assert_eq!(configs[0].1.runtime.cpus, 8);
        assert_eq!(configs[0].1.runtime.memory_mb, 4096);
        assert_eq!(configs[0].1.runtime.timeout_secs, 1200);
    }

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

    #[test]
    fn test_yaml_agent_field() {
        let yaml = r#"
defaults:
  image: base:latest
  agent: python-developer
sandboxes:
  test: {}
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        assert_eq!(configs[0].1.agent, Some("python-developer".to_string()));
    }

    #[test]
    fn test_yaml_skills_field() {
        let yaml = r#"
sandboxes:
  test:
    image: test:latest
    skills: [tdd, git-workflow]
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        assert_eq!(configs[0].1.skills, vec!["tdd", "git-workflow"]);
    }

    #[test]
    fn test_yaml_agent_per_sandbox_overrides_defaults() {
        let yaml = r#"
defaults:
  image: base:latest
  agent: python-developer
  skills: [tdd]
sandboxes:
  test:
    agent: rust-developer
    skills: [git-workflow, code-review]
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        assert_eq!(configs[0].1.agent, Some("rust-developer".to_string()));
        assert_eq!(configs[0].1.skills, vec!["git-workflow", "code-review"]);
    }

    #[test]
    fn test_yaml_agent_inherits_from_defaults() {
        let yaml = r#"
defaults:
  image: base:latest
  agent: python-developer
  skills: [tdd, git-workflow]
sandboxes:
  test: {}
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        assert_eq!(configs[0].1.agent, Some("python-developer".to_string()));
        assert_eq!(configs[0].1.skills, vec!["tdd", "git-workflow"]);
    }

    #[test]
    fn test_yaml_no_agent_no_skills() {
        let yaml = r#"
sandboxes:
  test:
    image: test:latest
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        assert!(configs[0].1.agent.is_none());
        assert!(configs[0].1.skills.is_empty());
    }

    #[test]
    fn test_yaml_agent_with_mcp_full_config() {
        let yaml = r#"
sandboxes:
  test:
    image: test:latest
    agent: python-developer
    skills: [tdd]
    mcp:
      github:
        command: npx
        args: ["-y", "@modelcontextprotocol/server-github"]
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        assert_eq!(configs[0].1.agent, Some("python-developer".to_string()));
        assert_eq!(configs[0].1.skills, vec!["tdd"]);
        assert!(configs[0].1.mcp_servers.contains_key("github"));
    }

    #[test]
    fn test_load_env_file_basic() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join(".env");
        std::fs::write(&env_path, "FOO=bar\nBAZ=qux\n").unwrap();

        let vars = load_env_file(".env", dir.path()).unwrap();
        assert_eq!(vars["FOO"], "bar");
        assert_eq!(vars["BAZ"], "qux");
    }

    #[test]
    fn test_load_env_file_comments_and_blanks() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("test.env");
        std::fs::write(&env_path, "# comment\n\nKEY=value\n  # another\n").unwrap();

        let vars = load_env_file("test.env", dir.path()).unwrap();
        assert_eq!(vars.len(), 1);
        assert_eq!(vars["KEY"], "value");
    }

    #[test]
    fn test_load_env_file_not_found() {
        let result = load_env_file("missing.env", std::path::Path::new("/tmp"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing.env"));
    }

    #[test]
    fn test_yaml_env_file_in_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("defaults.env"),
            "DEFAULT_KEY=default_value\n",
        )
        .unwrap();

        let yaml = r#"
defaults:
  env_file: defaults.env
sandboxes:
  test:
    image: alpine:latest
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs = resolve_sandbox_configs(&file, dir.path()).unwrap();
        assert_eq!(configs[0].1.runtime.env["DEFAULT_KEY"], "default_value");
    }

    #[test]
    fn test_yaml_env_file_per_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sandbox.env"), "SB_KEY=sb_value\n").unwrap();

        let yaml = r#"
sandboxes:
  test:
    image: alpine:latest
    env_file: sandbox.env
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs = resolve_sandbox_configs(&file, dir.path()).unwrap();
        assert_eq!(configs[0].1.runtime.env["SB_KEY"], "sb_value");
    }

    #[test]
    fn test_yaml_env_file_merge_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("defaults.env"),
            "SHARED=from_defaults_file\nDEFAULT_ONLY=yes\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("sandbox.env"),
            "SHARED=from_sandbox_file\nSB_ONLY=yes\n",
        )
        .unwrap();

        let yaml = r#"
defaults:
  env_file: defaults.env
  env:
    SHARED: from_defaults_env
sandboxes:
  test:
    image: alpine:latest
    env_file: sandbox.env
    env:
      SHARED: from_sandbox_env
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs = resolve_sandbox_configs(&file, dir.path()).unwrap();
        // Per-sandbox inline env has highest priority
        assert_eq!(configs[0].1.runtime.env["SHARED"], "from_sandbox_env");
        assert_eq!(configs[0].1.runtime.env["DEFAULT_ONLY"], "yes");
        assert_eq!(configs[0].1.runtime.env["SB_ONLY"], "yes");
    }

    #[test]
    fn test_yaml_agent_type_per_sandbox() {
        let yaml = r#"
sandboxes:
  test:
    image: test:latest
    type: claude
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        assert_eq!(
            configs[0].1.agent_type,
            Some(crate::config::AgentType::Claude)
        );
    }

    #[test]
    fn test_yaml_agent_type_invalid() {
        let yaml = r#"
sandboxes:
  test:
    image: test:latest
    type: invalid-agent
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let result = resolve_sandbox_configs(&file, std::path::Path::new("/tmp"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown agent type"));
    }

    #[test]
    fn test_yaml_model_per_sandbox() {
        let yaml = r#"
sandboxes:
  test:
    image: test:latest
    type: claude
    model: claude-sonnet-4-5-20250929
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        assert_eq!(
            configs[0].1.model,
            Some("claude-sonnet-4-5-20250929".to_string())
        );
    }

    #[test]
    fn test_yaml_model_inherited_from_defaults() {
        let yaml = r#"
defaults:
  model: claude-sonnet-4-5-20250929
sandboxes:
  test:
    image: test:latest
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        assert_eq!(
            configs[0].1.model,
            Some("claude-sonnet-4-5-20250929".to_string())
        );
    }

    #[test]
    fn test_yaml_model_per_sandbox_overrides_defaults() {
        let yaml = r#"
defaults:
  model: claude-sonnet-4-5-20250929
sandboxes:
  test:
    image: test:latest
    type: claude
    model: claude-opus-4-20250514
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        assert_eq!(
            configs[0].1.model,
            Some("claude-opus-4-20250514".to_string())
        );
    }

    #[test]
    fn test_yaml_invalid_model_for_agent_type() {
        // Set up a temporary models.json so validation is active.
        let dir = tempfile::tempdir().unwrap();
        let models_path = dir.path().join("models.json");
        std::fs::write(
            &models_path,
            r#"{"version":1,"agents":{"claude":{"models":["claude-opus-4-6"]}}}"#,
        )
        .unwrap();

        let registry = crate::config::models::ModelsRegistry::load_from(&models_path)
            .expect("test models.json should parse");

        // Manually validate to test the rejection path — resolve_sandbox_configs
        // only validates when a models config file exists at the default path.
        let result = registry.validate("claude", "gpt-4.1");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown model"));
    }

    #[test]
    fn test_yaml_model_without_type_skips_validation() {
        let yaml = r#"
sandboxes:
  test:
    image: test:latest
    model: any-model-value
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        assert_eq!(configs[0].1.model, Some("any-model-value".to_string()));
        assert!(configs[0].1.agent_type.is_none());
    }

    #[test]
    fn test_apply_cli_overrides_with_env() {
        let yaml = r#"
sandboxes:
  test:
    image: alpine:latest
    env:
      EXISTING: original
"#;
        let file = parse_sandbox_file(yaml).unwrap();
        let mut configs = resolve_sandbox_configs(&file, std::path::Path::new("/tmp")).unwrap();
        let cli_env = vec![
            ("NEW_KEY".to_string(), "new_value".to_string()),
            ("EXISTING".to_string(), "overridden".to_string()),
        ];
        apply_cli_overrides(&mut configs, None, None, None, None, &cli_env, None);
        assert_eq!(configs[0].1.runtime.env["NEW_KEY"], "new_value");
        assert_eq!(configs[0].1.runtime.env["EXISTING"], "overridden");
    }
}
