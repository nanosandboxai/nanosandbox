//! Sandbox configuration

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Configuration for creating a sandbox
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Unique name for the sandbox
    pub name: String,

    /// OCI image reference (e.g., "ghcr.io/nanosandboxai/agents-registry/claude:latest")
    pub image: String,

    /// CPU cores to allocate
    #[serde(default = "default_cpus")]
    pub cpus: u32,

    /// Memory in MB
    #[serde(default = "default_memory")]
    pub memory_mb: u32,

    /// Mount points
    #[serde(default)]
    pub mounts: Vec<Mount>,

    /// Environment variables
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Network configuration
    #[serde(default)]
    pub network: NetworkConfig,

    /// Working directory inside sandbox
    #[serde(default = "default_workdir")]
    pub workdir: String,

    /// Timeout in seconds
    #[serde(default = "default_timeout")]
    pub timeout_secs: u32,

    /// Optional project mount configuration.
    #[serde(default)]
    pub project: Option<ProjectConfig>,
}

fn default_cpus() -> u32 {
    1
}

fn default_memory() -> u32 {
    512
}

fn default_workdir() -> String {
    "/workspace".to_string()
}

fn default_timeout() -> u32 {
    300
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            image: String::new(),
            cpus: default_cpus(),
            memory_mb: default_memory(),
            mounts: Vec::new(),
            env: HashMap::new(),
            network: NetworkConfig::default(),
            workdir: default_workdir(),
            timeout_secs: default_timeout(),
            project: None,
        }
    }
}

impl SandboxConfig {
    /// Create a new config builder
    pub fn builder() -> SandboxConfigBuilder {
        SandboxConfigBuilder::default()
    }
}

/// Builder for SandboxConfig
#[derive(Debug, Default)]
pub struct SandboxConfigBuilder {
    config: SandboxConfig,
}

impl SandboxConfigBuilder {
    /// Set the sandbox name
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.config.name = name.into();
        self
    }

    /// Set the OCI image
    pub fn image(mut self, image: impl Into<String>) -> Self {
        self.config.image = image.into();
        self
    }

    /// Set CPU cores
    pub fn cpus(mut self, cpus: u32) -> Self {
        self.config.cpus = cpus;
        self
    }

    /// Set memory in MB
    pub fn memory_mb(mut self, memory_mb: u32) -> Self {
        self.config.memory_mb = memory_mb;
        self
    }

    /// Add a bind mount point
    pub fn mount(mut self, host: impl Into<PathBuf>, container: impl Into<String>) -> Self {
        self.config.mounts.push(Mount::bind(host, container));
        self
    }

    /// Add a readonly bind mount point
    pub fn mount_readonly(
        mut self,
        host: impl Into<PathBuf>,
        container: impl Into<String>,
    ) -> Self {
        self.config
            .mounts
            .push(Mount::bind(host, container).readonly());
        self
    }

    /// Add a virtio-fs mount point (faster for VMs)
    pub fn mount_virtiofs(
        mut self,
        host: impl Into<PathBuf>,
        container: impl Into<String>,
    ) -> Self {
        self.config.mounts.push(Mount::virtiofs(host, container));
        self
    }

    /// Add a readonly virtio-fs mount point
    pub fn mount_virtiofs_readonly(
        mut self,
        host: impl Into<PathBuf>,
        container: impl Into<String>,
    ) -> Self {
        self.config
            .mounts
            .push(Mount::virtiofs(host, container).readonly());
        self
    }

    /// Set the network mode
    pub fn network_mode(mut self, mode: NetworkMode) -> Self {
        self.config.network.mode = mode;
        self
    }

    /// Add a port mapping
    pub fn port(mut self, host: u16, container: u16) -> Self {
        self.config
            .network
            .port_mappings
            .push(PortMapping::tcp(host, container));
        self
    }

    /// Add a DNS server
    pub fn dns(mut self, server: impl Into<String>) -> Self {
        self.config.network.dns.push(server.into());
        self
    }

    /// Add a custom DNS zone entry for gvproxy's built-in resolver.
    pub fn dns_zone(mut self, name: impl Into<String>, ip: impl Into<String>) -> Self {
        self.config.network.dns_zones.insert(name.into(), ip.into());
        self
    }

    /// Add an environment variable
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.env.insert(key.into(), value.into());
        self
    }

    /// Set the working directory
    pub fn workdir(mut self, workdir: impl Into<String>) -> Self {
        self.config.workdir = workdir.into();
        self
    }

    /// Set the timeout
    pub fn timeout_secs(mut self, timeout: u32) -> Self {
        self.config.timeout_secs = timeout;
        self
    }

    /// Enable network access
    pub fn network_enabled(mut self, enabled: bool) -> Self {
        self.config.network.enabled = enabled;
        self
    }

    /// Set the TSI network scope (for libkrun direct FFI backend)
    pub fn network_scope(mut self, scope: NetworkScope) -> Self {
        self.config.network.scope = scope;
        self
    }

    /// Set a project directory to mount into the sandbox.
    pub fn project(mut self, path: impl Into<PathBuf>, branch: Option<&str>) -> Self {
        self.config.project = Some(ProjectConfig {
            path: path.into(),
            branch: branch.map(String::from),
            mount_point: "/workspace".to_string(),
            auto_sync: false,
        });
        self
    }

    /// Set a project directory with a specific branch name.
    pub fn project_with_branch(mut self, path: impl Into<PathBuf>, branch: &str) -> Self {
        self.config.project = Some(ProjectConfig {
            path: path.into(),
            branch: Some(branch.to_string()),
            mount_point: "/workspace".to_string(),
            auto_sync: false,
        });
        self
    }

    /// Build the configuration
    pub fn build(self) -> SandboxConfig {
        self.config
    }
}

/// Mount type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MountType {
    /// Standard bind mount
    #[default]
    Bind,
    /// virtio-fs mount (VM-optimized, faster)
    VirtioFs,
}

/// Mount point configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mount {
    /// Path on the host
    pub host_path: PathBuf,

    /// Path inside the sandbox
    pub container_path: String,

    /// Whether the mount is read-only
    #[serde(default)]
    pub readonly: bool,

    /// Mount type (bind or virtio-fs)
    #[serde(default)]
    pub mount_type: MountType,
}

impl Mount {
    /// Create a new bind mount
    pub fn bind(host: impl Into<PathBuf>, container: impl Into<String>) -> Self {
        Self {
            host_path: host.into(),
            container_path: container.into(),
            readonly: false,
            mount_type: MountType::Bind,
        }
    }

    /// Create a new virtio-fs mount
    pub fn virtiofs(host: impl Into<PathBuf>, container: impl Into<String>) -> Self {
        Self {
            host_path: host.into(),
            container_path: container.into(),
            readonly: false,
            mount_type: MountType::VirtioFs,
        }
    }

    /// Set mount as read-only
    pub fn readonly(mut self) -> Self {
        self.readonly = true;
        self
    }
}

/// Port mapping for inbound connections
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortMapping {
    /// Port on the host
    pub host_port: u16,
    /// Port inside the container
    pub container_port: u16,
    /// Protocol (tcp/udp)
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

fn default_protocol() -> String {
    "tcp".to_string()
}

impl PortMapping {
    /// Create a TCP port mapping
    pub fn tcp(host: u16, container: u16) -> Self {
        Self {
            host_port: host,
            container_port: container,
            protocol: "tcp".to_string(),
        }
    }

    /// Create a UDP port mapping
    pub fn udp(host: u16, container: u16) -> Self {
        Self {
            host_port: host,
            container_port: container,
            protocol: "udp".to_string(),
        }
    }
}

/// Network configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Whether network access is enabled
    #[serde(default = "default_network_enabled")]
    pub enabled: bool,

    /// Network mode (tsi, bridge, none)
    #[serde(default = "default_network_mode")]
    pub mode: NetworkMode,

    /// TSI network scope (used by libkrun direct FFI backend)
    /// Controls what network destinations the VM can reach.
    #[serde(default = "default_network_scope")]
    pub scope: NetworkScope,

    /// Port mappings for inbound connections
    #[serde(default)]
    pub port_mappings: Vec<PortMapping>,

    /// DNS servers (uses host DNS if empty)
    #[serde(default)]
    pub dns: Vec<String>,

    /// Custom DNS zone entries (name -> IP address).
    /// Configured in gvproxy's built-in DNS resolver after startup.
    /// Example: `"containers.internal" -> "192.168.127.1"`
    #[serde(default)]
    pub dns_zones: HashMap<String, String>,
}

fn default_network_enabled() -> bool {
    true
}

fn default_network_mode() -> NetworkMode {
    NetworkMode::Tsi
}

fn default_network_scope() -> NetworkScope {
    NetworkScope::Any
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            enabled: default_network_enabled(),
            mode: default_network_mode(),
            scope: default_network_scope(),
            port_mappings: Vec::new(),
            dns: Vec::new(),
            dns_zones: HashMap::new(),
        }
    }
}

impl NetworkConfig {
    /// Create a config with no network access
    pub fn none() -> Self {
        Self {
            enabled: false,
            mode: NetworkMode::None,
            scope: NetworkScope::None,
            port_mappings: Vec::new(),
            dns: Vec::new(),
            dns_zones: HashMap::new(),
        }
    }

    /// Create a config with TSI networking (full internet access)
    pub fn tsi() -> Self {
        Self {
            enabled: true,
            mode: NetworkMode::Tsi,
            scope: NetworkScope::Any,
            port_mappings: Vec::new(),
            dns: Vec::new(),
            dns_zones: HashMap::new(),
        }
    }

    /// Add a port mapping
    pub fn with_port(mut self, host: u16, container: u16) -> Self {
        self.port_mappings.push(PortMapping::tcp(host, container));
        self
    }

    /// Add a DNS server
    pub fn with_dns(mut self, server: impl Into<String>) -> Self {
        self.dns.push(server.into());
        self
    }

    /// Add a custom DNS zone entry for gvproxy's built-in resolver.
    pub fn with_dns_zone(mut self, name: impl Into<String>, ip: impl Into<String>) -> Self {
        self.dns_zones.insert(name.into(), ip.into());
        self
    }
}

/// Network mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    /// No network access
    None,

    /// Transparent Socket Impersonation (default)
    #[default]
    Tsi,

    /// Virtual bridge network
    Bridge,
}

/// TSI Network Scope for libkrun
///
/// Controls the level of network access the VM has when using
/// TSI (Transparent Socket Impersonation) via direct libkrun FFI.
///
/// libkrun enables TSI networking by default (when no explicit network
/// device is added). This enum allows configuring the desired scope
/// for future use with `krun_add_vsock` TSI feature flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NetworkScope {
    /// No outbound network access
    None,

    /// Local group only
    Group,

    /// Public internet access
    Public,

    /// Full access including localhost - recommended default
    #[default]
    Any,
}

/// Registry configuration for per-registry settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryConfig {
    /// Registry hostname (e.g., "ghcr.io")
    pub host: String,

    /// Allow insecure HTTP connections
    #[serde(default)]
    pub insecure: bool,

    /// Skip TLS certificate verification
    #[serde(default)]
    pub skip_tls_verify: bool,
}

impl RegistryConfig {
    /// Create a new registry config
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            insecure: false,
            skip_tls_verify: false,
        }
    }

    /// Allow insecure HTTP connections
    pub fn insecure(mut self) -> Self {
        self.insecure = true;
        self
    }

    /// Skip TLS verification
    pub fn skip_tls(mut self) -> Self {
        self.skip_tls_verify = true;
        self
    }
}

/// Configuration for mounting a project into the sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// Host path to the project directory.
    pub path: PathBuf,
    /// Optional branch name (auto-generated if None).
    pub branch: Option<String>,
    /// Mount point inside the VM (default: /workspace).
    #[serde(default = "default_project_mount_point")]
    pub mount_point: String,
    /// Whether auto-sync is enabled. When false, clones are created without source branches.
    #[serde(default)]
    pub auto_sync: bool,
}

fn default_project_mount_point() -> String {
    "/workspace".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_config_with_project() {
        let config = SandboxConfig::builder()
            .name("test")
            .image("alpine")
            .project("/tmp/myproject", None)
            .build();

        assert!(config.project.is_some());
        let proj = config.project.unwrap();
        assert_eq!(proj.path, std::path::PathBuf::from("/tmp/myproject"));
        assert!(proj.branch.is_none());
        assert_eq!(proj.mount_point, "/workspace");
    }

    #[test]
    fn test_sandbox_config_with_project_and_branch() {
        let config = SandboxConfig::builder()
            .name("test")
            .image("alpine")
            .project_with_branch("/tmp/myproject", "feat/auth")
            .build();

        let proj = config.project.unwrap();
        assert_eq!(proj.branch, Some("feat/auth".to_string()));
    }

    #[test]
    fn test_sandbox_config_no_project() {
        let config = SandboxConfig::builder()
            .name("test")
            .image("alpine")
            .build();

        assert!(config.project.is_none());
    }

}
