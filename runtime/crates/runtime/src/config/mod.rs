//! Sandbox configuration

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Runtime mode selection.
///
/// - `Legacy` (default): Current behavior — boots with nanosb-init.sh wrapper,
///   gateway agent, SSH keys, etc.
/// - `Next`: New zero-image-customization mode — boots a vanilla image with
///   console I/O, extra virtiofs mounts, and network bring-up at the microVM
///   layer. No nanosb artifacts in the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    /// Legacy gateway-based mode (default)
    #[default]
    Legacy,
    /// Next-gen mode: vanilla images, console I/O, microVM-layer setup
    Next,
}

/// Console I/O specification for the "next" runtime mode.
///
/// Describes how host-side file descriptors are wired to the VM's virtio-console.
/// In next mode, the `internal-boot-vm` subprocess receives these fds via
/// inheritance from the parent process (the supervisor).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleSpec {
    /// Host-side fd number for stdin input to the VM (0 = inherit parent stdin).
    /// The subprocess reads from this fd and writes to the virtio-console input.
    pub stdin_fd: i32,
    /// Host-side fd number for stdout output from the VM (1 = inherit parent stdout).
    /// The subprocess reads from virtio-console output and writes to this fd.
    pub stdout_fd: i32,
    /// Host-side fd number for stderr output from the VM (2 = inherit parent stderr).
    pub stderr_fd: i32,
    /// Whether to set up a TTY on the console (enables raw mode, SIGWINCH resize).
    #[serde(default)]
    pub tty: bool,
}

impl Default for ConsoleSpec {
    fn default() -> Self {
        Self {
            stdin_fd: 0,
            stdout_fd: 1,
            stderr_fd: 2,
            tty: false,
        }
    }
}

/// An extra virtiofs mount to be set up at the microVM layer (next mode only).
///
/// The host registers the tag via `krun_add_virtiofs`; the guest-side mount
/// is performed by the libkrun init patch (see `runtime/scripts/patches/`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtraMount {
    /// virtiofs tag (must match the tag registered via `krun_add_virtiofs`)
    pub tag: String,
    /// Host-side source path for the virtiofs share.
    /// This is the path on the host filesystem that will be shared.
    pub host_path: String,
    /// Target mount point inside the guest (e.g., "/mnt/config")
    pub target: String,
    /// Whether the mount is read-only
    #[serde(default)]
    pub readonly: bool,
}

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

    /// Run agent commands as root user inside the guest.
    ///
    /// Accepts both snake_case (`run_as_root`) and camelCase (`runAsRoot`)
    /// in serialized config formats.
    #[serde(default, alias = "runAsRoot")]
    pub run_as_root: bool,

    /// Optional project mount configuration.
    #[serde(default)]
    pub project: Option<ProjectConfig>,

    /// SSH public key to pass via kernel cmdline for FUSE mode.
    /// Set internally by the sandbox layer; not exposed to users.
    #[serde(default, skip_serializing)]
    pub ssh_pubkey: Option<String>,

    /// PID 1 command to run inside the VM.
    /// Set by the sandbox layer before calling start().
    /// If None, the runtime falls back to a sleep hold command.
    #[serde(default)]
    pub command: Option<String>,

    /// Arguments for the PID 1 command.
    #[serde(default)]
    pub command_args: Vec<String>,

    /// Runtime mode: Legacy (default) or Next (zero-image-customization).
    #[serde(default)]
    pub runtime_mode: RuntimeMode,

    /// Console I/O specification for next mode.
    /// Ignored in legacy mode.
    #[serde(default, skip_serializing)]
    pub console: Option<ConsoleSpec>,

    /// Extra virtiofs mounts for next mode (tag, target, readonly).
    /// These are registered via `krun_add_virtiofs` on the host side;
    /// guest-side mounting is handled by the libkrun init patch.
    /// Ignored in legacy mode.
    #[serde(default, skip_serializing)]
    pub extra_mounts: Vec<ExtraMount>,

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
            run_as_root: false,
            project: None,
            ssh_pubkey: None,
            command: None,
            command_args: Vec::new(),
            runtime_mode: RuntimeMode::default(),
            console: None,
            extra_mounts: Vec::new(),
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

    /// Set whether agent commands run as root in guest.
    pub fn run_as_root(mut self, run_as_root: bool) -> Self {
        self.config.run_as_root = run_as_root;
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

    /// Set the runtime mode (Legacy or Next).
    pub fn runtime_mode(mut self, mode: RuntimeMode) -> Self {
        self.config.runtime_mode = mode;
        self
    }

    /// Set the console specification for next mode.
    pub fn console(mut self, console: ConsoleSpec) -> Self {
        self.config.console = Some(console);
        self
    }

    /// Add an extra virtiofs mount for next mode.
    pub fn extra_mount(mut self, tag: impl Into<String>, host_path: impl Into<String>, target: impl Into<String>, readonly: bool) -> Self {
        self.config.extra_mounts.push(ExtraMount {
            tag: tag.into(),
            host_path: host_path.into(),
            target: target.into(),
            readonly,
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

    // ── RuntimeMode tests ───────────────────────────────────────────────

    #[test]
    fn test_runtime_mode_default_is_legacy() {
        let config = SandboxConfig::builder()
            .name("test")
            .image("alpine")
            .build();
        assert_eq!(config.runtime_mode, RuntimeMode::Legacy);
    }

    #[test]
    fn test_runtime_mode_next() {
        let config = SandboxConfig::builder()
            .name("test")
            .image("alpine")
            .runtime_mode(RuntimeMode::Next)
            .build();
        assert_eq!(config.runtime_mode, RuntimeMode::Next);
    }

    #[test]
    fn test_runtime_mode_serde_snake_case() {
        let json = r#"{"name": "test", "image": "alpine", "runtime_mode": "next"}"#;
        let config: SandboxConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.runtime_mode, RuntimeMode::Next);

        let json = r#"{"name": "test", "image": "alpine", "runtime_mode": "legacy"}"#;
        let config: SandboxConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.runtime_mode, RuntimeMode::Legacy);
    }

    // ── ConsoleSpec tests ───────────────────────────────────────────────

    #[test]
    fn test_console_spec_default() {
        let spec = ConsoleSpec::default();
        assert_eq!(spec.stdin_fd, 0);
        assert_eq!(spec.stdout_fd, 1);
        assert_eq!(spec.stderr_fd, 2);
        assert!(!spec.tty);
    }

    #[test]
    fn test_console_spec_custom() {
        let spec = ConsoleSpec {
            stdin_fd: 10,
            stdout_fd: 11,
            stderr_fd: 12,
            tty: true,
        };
        assert_eq!(spec.stdin_fd, 10);
        assert_eq!(spec.stdout_fd, 11);
        assert_eq!(spec.stderr_fd, 12);
        assert!(spec.tty);
    }

    #[test]
    fn test_console_spec_serde_roundtrip() {
        let spec = ConsoleSpec {
            stdin_fd: 3,
            stdout_fd: 4,
            stderr_fd: 5,
            tty: true,
        };
        let json = serde_json::to_string(&spec).unwrap();
        let deserialized: ConsoleSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.stdin_fd, 3);
        assert_eq!(deserialized.stdout_fd, 4);
        assert_eq!(deserialized.stderr_fd, 5);
        assert!(deserialized.tty);
    }

    #[test]
    fn test_console_spec_serde_default_tty() {
        let json = r#"{"stdin_fd": 0, "stdout_fd": 1, "stderr_fd": 2}"#;
        let spec: ConsoleSpec = serde_json::from_str(json).unwrap();
        assert!(!spec.tty); // tty defaults to false
    }

    // ── ExtraMount tests ────────────────────────────────────────────────

    #[test]
    fn test_extra_mount_default_readonly() {
        let mount = ExtraMount {
            tag: "test".to_string(),
            host_path: "/host/test".to_string(),
            target: "/mnt/test".to_string(),
            readonly: false,
        };
        assert!(!mount.readonly);
    }

    #[test]
    fn test_extra_mount_readonly() {
        let mount = ExtraMount {
            tag: "config".to_string(),
            host_path: "/host/config".to_string(),
            target: "/mnt/config".to_string(),
            readonly: true,
        };
        assert!(mount.readonly);
    }

    #[test]
    fn test_extra_mount_serde_roundtrip() {
        let mount = ExtraMount {
            tag: "workspace".to_string(),
            host_path: "/host/workspace".to_string(),
            target: "/mnt/workspace".to_string(),
            readonly: false,
        };
        let json = serde_json::to_string(&mount).unwrap();
        let deserialized: ExtraMount = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.tag, "workspace");
        assert_eq!(deserialized.host_path, "/host/workspace");
        assert_eq!(deserialized.target, "/mnt/workspace");
        assert!(!deserialized.readonly);
    }

    #[test]
    fn test_extra_mount_serde_readonly_default() {
        let json = r#"{"tag": "data", "host_path": "/host/data", "target": "/mnt/data"}"#;
        let mount: ExtraMount = serde_json::from_str(json).unwrap();
        assert_eq!(mount.host_path, "/host/data");
        assert!(!mount.readonly); // readonly defaults to false
    }

    // ── SandboxConfig next-mode builder tests ───────────────────────────

    #[test]
    fn test_sandbox_config_next_mode_with_console() {
        let console = ConsoleSpec {
            stdin_fd: 10,
            stdout_fd: 11,
            stderr_fd: 12,
            tty: true,
        };
        let config = SandboxConfig::builder()
            .name("next-test")
            .image("alpine")
            .runtime_mode(RuntimeMode::Next)
            .console(console)
            .build();

        assert_eq!(config.runtime_mode, RuntimeMode::Next);
        assert!(config.console.is_some());
        let c = config.console.unwrap();
        assert_eq!(c.stdin_fd, 10);
        assert!(c.tty);
    }

    #[test]
    fn test_sandbox_config_next_mode_with_extra_mounts() {
        let config = SandboxConfig::builder()
            .name("mount-test")
            .image("alpine")
            .runtime_mode(RuntimeMode::Next)
            .extra_mount("config", "/host/config", "/mnt/config", true)
            .extra_mount("workspace", "/host/workspace", "/mnt/workspace", false)
            .build();

        assert_eq!(config.extra_mounts.len(), 2);
        assert_eq!(config.extra_mounts[0].tag, "config");
        assert_eq!(config.extra_mounts[0].host_path, "/host/config");
        assert!(config.extra_mounts[0].readonly);
        assert_eq!(config.extra_mounts[1].tag, "workspace");
        assert_eq!(config.extra_mounts[1].host_path, "/host/workspace");
        assert!(!config.extra_mounts[1].readonly);
    }

    #[test]
    fn test_sandbox_config_legacy_ignores_next_fields() {
        // Legacy mode should not set console or extra_mounts by default
        let config = SandboxConfig::builder()
            .name("legacy-test")
            .image("alpine")
            .build();

        assert_eq!(config.runtime_mode, RuntimeMode::Legacy);
        assert!(config.console.is_none());
        assert!(config.extra_mounts.is_empty());
    }
}
