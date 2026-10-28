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

/// How the host project directory is exposed to a sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    /// The agent works on a host-owned clone; the host reviews changes before
    /// applying them, and never runs tools on the source repo (default).
    #[default]
    Isolated,
    /// Mount the source project directory read-write directly (legacy opt-in).
    Shared,
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

    /// Deprecated alias for `user = "0"`; use [`SandboxConfig::user`].
    ///
    /// Kept for `sandbox.yml` / ABI back-compat; emits a deprecation warning.
    ///
    /// Accepts both snake_case (`run_as_root`) and camelCase (`runAsRoot`)
    /// in serialized config formats.
    #[serde(default, alias = "runAsRoot")]
    pub run_as_root: bool,

    /// User to run the PID 1 command as inside the guest.
    ///
    /// Accepts `"UID"`, `"UID:GID"`, or a username (resolved in the guest).
    /// When unset, the command runs as root.
    #[serde(default)]
    pub user: Option<String>,

    /// Home directory to set for the guest command (`HOME`).
    ///
    /// When unset and `user` is a name, defaults to `/home/<user>`.
    #[serde(default)]
    pub home: Option<String>,

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

    /// Path to the in-guest exec agent to run *alongside* the PID-1 workload
    /// (next mode only). When set, the runtime exports `KRUN_EXEC_AGENT` so the
    /// guest init backgrounds the agent at that path, keeping `command` as the
    /// workload. Set by the host layer when the exec channel is enabled.
    #[serde(default)]
    pub exec_agent: Option<String>,

    /// Runtime mode: Legacy (default) or Next (zero-image-customization).
    #[serde(default)]
    pub runtime_mode: RuntimeMode,

    /// Host project sharing mode (default `isolated`).
    #[serde(default)]
    pub workspace_mode: WorkspaceMode,

    /// Max inodes the guest may create (0 = unlimited). Opt-in.
    #[serde(default)]
    pub max_inodes: u64,
    /// Max processes the guest may run (0 = unlimited). Opt-in.
    #[serde(default)]
    pub max_pids: u32,
    /// Guest disk I/O limit in bytes/sec (0 = unlimited). Opt-in.
    #[serde(default)]
    pub io_bps: u64,

    /// Give each boot a copy-on-write clone of the rootfs so guest writes never
    /// dirty the pristine template (default false = the template is the boot root).
    #[serde(default)]
    pub fresh_rootfs: bool,

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

    /// Host-side unix socket path bridged to a guest vsock port (next mode).
    ///
    /// When set, libkrun listens on this host socket and forwards connections to
    /// `vsock_port` inside the guest — the dedicated host↔guest exec channel.
    #[serde(default)]
    pub vsock_socket: Option<String>,

    /// Guest vsock port the host socket is bridged to.
    #[serde(default)]
    pub vsock_port: Option<u32>,

    /// Arbitrary key/value labels for attribution and filtering.
    #[serde(default)]
    pub labels: HashMap<String, String>,

    /// Ephemeral sandboxes are removed automatically once they stop.
    #[serde(default)]
    pub ephemeral: bool,

    /// Guest hostname override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,

    /// Non-OCI rootfs source (local dir / disk image).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rootfs: Option<RootfsConfig>,

    /// Pre-boot rootfs patches applied host-side before boot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patches: Vec<RootfsPatchConfig>,

    /// Named scripts materialized at `/.nanosb/scripts/<name>` before boot.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub scripts: HashMap<String, String>,

    /// Per-registry pull configuration (insecure / skip-TLS / credentials).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub registry: Vec<RegistryConfig>,

    /// Stat virtualization mode (`strict`/`relaxed`/`off`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stat_virtualization: Option<String>,

    /// Guest security profile (`default`/`restricted`).
    #[serde(default = "default_security_profile")]
    pub security_profile: String,

    /// VM-level rlimits passed to the guest init (`RESOURCE=soft:hard`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rlimits: Vec<String>,

    /// Docker-in-VM configuration (Epic 7 D5). None means disabled and changes nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker: Option<DockerConfig>,
}

fn default_security_profile() -> String {
    "default".to_string()
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
            user: None,
            home: None,
            project: None,
            ssh_pubkey: None,
            command: None,
            command_args: Vec::new(),
            exec_agent: None,
            runtime_mode: RuntimeMode::default(),
            workspace_mode: WorkspaceMode::default(),
            max_inodes: 0,
            max_pids: 0,
            io_bps: 0,
            fresh_rootfs: false,
            console: None,
            extra_mounts: Vec::new(),
            vsock_socket: None,
            vsock_port: None,
            labels: HashMap::new(),
            ephemeral: false,
            hostname: None,
            rootfs: None,
            patches: Vec::new(),
            scripts: HashMap::new(),
            registry: Vec::new(),
            stat_virtualization: None,
            security_profile: default_security_profile(),
            rlimits: Vec::new(),
            docker: None,
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

    /// Set the host project sharing mode
    pub fn workspace_mode(mut self, mode: WorkspaceMode) -> Self {
        self.config.workspace_mode = mode;
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

    /// Set the guest user to run the command as (`UID`, `UID:GID`, or username).
    pub fn user(mut self, user: impl Into<String>) -> Self {
        self.config.user = Some(user.into());
        self
    }

    /// Set the guest home directory (`HOME`) for the command.
    pub fn home(mut self, home: impl Into<String>) -> Self {
        self.config.home = Some(home.into());
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
    pub fn extra_mount(
        mut self,
        tag: impl Into<String>,
        host_path: impl Into<String>,
        target: impl Into<String>,
        readonly: bool,
    ) -> Self {
        self.config.extra_mounts.push(ExtraMount {
            tag: tag.into(),
            host_path: host_path.into(),
            target: target.into(),
            readonly,
        });
        self
    }

    /// Bridge a host unix socket to a guest vsock port (next mode).
    pub fn vsock_bridge(mut self, socket_path: impl Into<String>, guest_port: u32) -> Self {
        self.config.vsock_socket = Some(socket_path.into());
        self.config.vsock_port = Some(guest_port);
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
    /// Guest tmpfs (host_path ignored).
    Tmpfs,
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

    /// Mount type (bind, virtio-fs, or tmpfs)
    #[serde(default)]
    pub mount_type: MountType,

    /// Single file shared from the host (share parent dir + bind the file).
    #[serde(default)]
    pub file: bool,

    /// Disk-image file hosted as a block device (format `raw`/`qcow2`/`vmdk`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_image: Option<String>,

    /// Filesystem used to mount a `disk_image` block device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fstype: Option<String>,

    /// Guest mount options (`noexec`, `nosuid`, `nodev`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,

    /// Guest uid owning the mount point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,

    /// Guest gid owning the mount point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
}

impl Mount {
    /// Create a new bind mount
    pub fn bind(host: impl Into<PathBuf>, container: impl Into<String>) -> Self {
        Self {
            host_path: host.into(),
            container_path: container.into(),
            readonly: false,
            mount_type: MountType::Bind,
            file: false,
            disk_image: None,
            fstype: None,
            options: Vec::new(),
            uid: None,
            gid: None,
        }
    }

    /// Create a new virtio-fs mount
    pub fn virtiofs(host: impl Into<PathBuf>, container: impl Into<String>) -> Self {
        Self {
            mount_type: MountType::VirtioFs,
            ..Self::bind(host, container)
        }
    }

    /// Create a guest tmpfs mount.
    pub fn tmpfs(container: impl Into<String>) -> Self {
        Self {
            container_path: container.into(),
            mount_type: MountType::Tmpfs,
            ..Self::bind("/", "")
        }
    }

    /// Set mount as read-only
    pub fn readonly(mut self) -> Self {
        self.readonly = true;
        self
    }
}

/// Source of the guest rootfs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RootfsSource {
    /// Extract the OCI `image`.
    #[default]
    Oci,
    /// Boot directly from a host directory.
    LocalDir,
    /// Boot from a disk-image attached as a block device.
    DiskImage,
}

/// Non-OCI rootfs configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootfsConfig {
    #[serde(default)]
    pub source: RootfsSource,
    /// Host directory (`local_dir`) or disk-image file (`disk_image`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fstype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default)]
    pub read_only: bool,
}

/// A pre-boot rootfs patch applied host-side before boot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RootfsPatchConfig {
    Mkdir {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<u32>,
    },
    Text {
        path: String,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<u32>,
    },
    CopyFile {
        from: String,
        to: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<u32>,
    },
    CopyDir {
        from: String,
        to: String,
    },
    Remove {
        path: String,
        #[serde(default)]
        recursive: bool,
    },
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
    /// Explicit host bind address; loopback by default. Config surface only —
    /// enforcement is Epic 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind_address: Option<String>,
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
            bind_address: None,
        }
    }

    /// Create a UDP port mapping
    pub fn udp(host: u16, container: u16) -> Self {
        Self {
            host_port: host,
            container_port: container,
            protocol: "udp".to_string(),
            bind_address: None,
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

    /// Opaque network policy config passthrough. Carried through to guests and
    /// consumers; enforcement is a separate engine (Epic 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<serde_json::Value>,
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
            policy: None,
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
            policy: None,
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
            policy: None,
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

/// Docker-in-VM runtime selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DockerRuntime {
    /// Docker daemon (default)
    #[default]
    Dockerd,
    /// Podman
    Podman,
    /// containerd
    Containerd,
}

/// Docker storage driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DockerStorageDriver {
    /// Overlay2 (default)
    #[default]
    Overlay2,
    /// VFS
    Vfs,
    /// FUSE overlayfs
    #[serde(rename = "fuse-overlayfs")]
    FuseOverlayfs,
}

/// Docker network mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DockerNetwork {
    /// Host networking (default)
    #[default]
    Host,
    /// No networking
    None,
    /// Bridge networking
    Bridge,
}

/// Docker-in-VM configuration (Epic 7 D5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DockerConfig {
    /// Whether the Docker daemon is enabled in the VM.
    #[serde(default)]
    pub enabled: bool,
    /// Container runtime to use.
    #[serde(default = "default_docker_runtime")]
    pub runtime: DockerRuntime,
    /// Storage driver for the Docker daemon.
    #[serde(default = "default_docker_storage_driver")]
    pub storage_driver: DockerStorageDriver,
    /// Data volume size in bytes.
    #[serde(default = "default_docker_data_size")]
    pub data_size_bytes: u64,
    /// Docker network mode.
    #[serde(default = "default_docker_network")]
    pub network: DockerNetwork,
    /// Extra daemon.json overrides.
    #[serde(default)]
    pub daemon_json: HashMap<String, serde_json::Value>,
    /// Images to preload into the VM.
    #[serde(default)]
    pub preload_images: Vec<String>,
    /// Whether to run the Docker daemon rootless.
    #[serde(default)]
    pub rootless: bool,
    /// Docker socket path inside the VM.
    #[serde(default = "default_docker_socket")]
    pub socket: String,
}

fn default_docker_runtime() -> DockerRuntime {
    DockerRuntime::Dockerd
}

fn default_docker_storage_driver() -> DockerStorageDriver {
    DockerStorageDriver::Overlay2
}

fn default_docker_data_size() -> u64 {
    20 * 1024 * 1024 * 1024
}

fn default_docker_network() -> DockerNetwork {
    DockerNetwork::Host
}

fn default_docker_socket() -> String {
    "/var/run/docker.sock".to_string()
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            runtime: default_docker_runtime(),
            storage_driver: default_docker_storage_driver(),
            data_size_bytes: default_docker_data_size(),
            network: default_docker_network(),
            daemon_json: HashMap::new(),
            preload_images: Vec::new(),
            rootless: false,
            socket: default_docker_socket(),
        }
    }
}

pub const DAEMON_JSON_ALLOWED_KEYS: &[&str] = &[
    "log-driver",
    "log-level",
    "log-opts",
    "registry-mirrors",
    "insecure-registries",
    "dns",
    "dns-opts",
    "dns-search",
    "mtu",
    "default-address-pools",
    "max-concurrent-downloads",
    "max-concurrent-uploads",
    "max-download-attempts",
    "shutdown-timeout",
    "default-ulimits",
    "labels",
    "features",
    "live-restore",
];

pub const DAEMON_JSON_FORBIDDEN_KEYS: &[&str] = &[
    "bridge",
    "bip",
    "fixed-cidr",
    "fixed-cidr-v6",
    "iptables",
    "ip6tables",
    "ip-forward",
    "ip-masq",
    "data-root",
    "storage-driver",
    "hosts",
    "tls",
    "tlsverify",
    "tlscacert",
    "tlscert",
    "tlskey",
    "authorization-plugins",
    "default-runtime",
    "runtimes",
    "exec-opts",
    "seccomp-profile",
    "apparmor-profile",
    "selinux-enabled",
    "userland-proxy",
    "userns-remap",
    "experimental",
];

impl DockerConfig {
    /// Validate this block against the Tier-1 Docker constraints. Returns a
    /// human-readable error (prefixed `docker:`) the caller surfaces loudly.
    /// A disabled block never fails: it has no runtime effect.
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if self.rootless {
            return Err(
                "docker: rootless is not supported in Tier 1; set rootless: false (or omit it)"
                    .to_string(),
            );
        }
        for (key, value) in &self.daemon_json {
            if DAEMON_JSON_FORBIDDEN_KEYS.contains(&key.as_str()) {
                return Err(format!(
                    "docker: daemon_json key '{key}' is forbidden in Tier 1 (it would widen or subvert the Tier-1 isolation surface)"
                ));
            }
            if !DAEMON_JSON_ALLOWED_KEYS.contains(&key.as_str()) {
                return Err(format!(
                    "docker: daemon_json key '{key}' is not allowed in Tier 1"
                ));
            }
            if key == "registry-mirrors" || key == "insecure-registries" {
                let Some(entries) = value.as_array() else {
                    return Err(format!(
                        "docker: daemon_json '{key}' must be an array of strings"
                    ));
                };
                for entry in entries {
                    let Some(s) = entry.as_str() else {
                        return Err(format!(
                            "docker: daemon_json '{key}' entries must be strings"
                        ));
                    };
                    if url_has_userinfo(s) {
                        return Err(format!(
                            "docker: daemon_json '{key}' entry must not embed credentials"
                        ));
                    }
                }
            }
        }
        for image in &self.preload_images {
            if image.is_empty() {
                return Err("docker: preload_images entries must not be empty".to_string());
            }
            if image.contains("://") {
                return Err(format!(
                    "docker: preload image '{image}' must not be a URL or embed credentials"
                ));
            }
            if let Some(at) = image.find('@') {
                let before_at = &image[..at];
                let is_before_first_slash = match image.find('/') {
                    Some(slash) => at < slash,
                    None => true,
                };
                if is_before_first_slash && before_at.contains(':') {
                    return Err(format!(
                        "docker: preload image '{image}' must not embed credentials"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// True when a registry URL embeds `userinfo@` (i.e. `scheme://user:pass@host`).
fn url_has_userinfo(s: &str) -> bool {
    let rest = s.split_once("://").map(|(_, r)| r).unwrap_or(s);
    let authority = rest.split('/').next().unwrap_or(rest);
    authority.contains('@')
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

    #[test]
    fn test_workspace_mode_default_is_isolated() {
        let config = SandboxConfig::builder().name("t").image("alpine").build();
        assert_eq!(config.workspace_mode, WorkspaceMode::Isolated);
    }

    #[test]
    fn test_workspace_mode_serde_snake_case() {
        let json = r#"{"name": "t", "image": "alpine", "workspace_mode": "shared"}"#;
        let config: SandboxConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.workspace_mode, WorkspaceMode::Shared);

        let json = r#"{"name": "t", "image": "alpine", "workspace_mode": "isolated"}"#;
        let config: SandboxConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.workspace_mode, WorkspaceMode::Isolated);
    }

    #[test]
    fn test_quota_defaults_are_unlimited() {
        let config = SandboxConfig::builder().name("t").image("alpine").build();
        assert_eq!(config.max_inodes, 0);
        assert_eq!(config.max_pids, 0);
        assert_eq!(config.io_bps, 0);
    }

    #[test]
    fn test_quota_serde() {
        let json = r#"{"name": "t", "image": "a", "max_inodes": 10000, "max_pids": 512, "io_bps": 1048576}"#;
        let c: SandboxConfig = serde_json::from_str(json).unwrap();
        assert_eq!((c.max_inodes, c.max_pids, c.io_bps), (10000, 512, 1048576));
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
    fn test_port_mapping_bind_address_roundtrip() {
        let m = PortMapping {
            host_port: 8080,
            container_port: 80,
            protocol: "tcp".to_string(),
            bind_address: Some("127.0.0.1".to_string()),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains(r#""bind_address":"127.0.0.1""#));
        let back: PortMapping = serde_json::from_str(&json).unwrap();
        assert_eq!(back.bind_address.as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn test_port_mapping_bind_address_defaults_none() {
        let json = r#"{"host_port": 8080, "container_port": 80, "protocol": "tcp"}"#;
        let m: PortMapping = serde_json::from_str(json).unwrap();
        assert!(m.bind_address.is_none());
        let tcp = PortMapping::tcp(8080, 80);
        assert!(tcp.bind_address.is_none());
        let udp = PortMapping::udp(53, 53);
        assert!(udp.bind_address.is_none());
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

    // ── DockerConfig tests ─────────────────────────────────────────────

    #[test]
    fn test_docker_config_defaults() {
        let cfg = DockerConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.runtime, DockerRuntime::Dockerd);
        assert_eq!(cfg.storage_driver, DockerStorageDriver::Overlay2);
        assert_eq!(cfg.data_size_bytes, 20 * 1024 * 1024 * 1024);
        assert_eq!(cfg.network, DockerNetwork::Host);
        assert!(cfg.daemon_json.is_empty());
        assert!(cfg.preload_images.is_empty());
        assert!(!cfg.rootless);
        assert_eq!(cfg.socket, "/var/run/docker.sock");
    }

    #[test]
    fn test_docker_config_serde_roundtrip() {
        let cfg = DockerConfig {
            enabled: true,
            runtime: DockerRuntime::Podman,
            storage_driver: DockerStorageDriver::Vfs,
            data_size_bytes: 40 * 1024 * 1024 * 1024,
            network: DockerNetwork::Bridge,
            daemon_json: HashMap::from([(
                "log-level".to_string(),
                serde_json::from_str::<serde_json::Value>(r#""debug""#).unwrap(),
            )]),
            preload_images: vec!["alpine".to_string()],
            rootless: true,
            socket: "/run/docker.sock".to_string(),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: DockerConfig = serde_json::from_str(&json).unwrap();
        assert!(back.enabled);
        assert_eq!(back.runtime, DockerRuntime::Podman);
        assert_eq!(back.storage_driver, DockerStorageDriver::Vfs);
        assert_eq!(back.data_size_bytes, 40 * 1024 * 1024 * 1024);
        assert_eq!(back.network, DockerNetwork::Bridge);
        assert_eq!(
            serde_json::to_string(back.daemon_json.get("log-level").unwrap()).unwrap(),
            r#""debug""#,
        );
        assert_eq!(back.preload_images, vec!["alpine".to_string()]);
        assert!(back.rootless);
        assert_eq!(back.socket, "/run/docker.sock");
    }

    #[test]
    fn test_docker_config_storage_driver_fuse_overlayfs_wire() {
        let mut cfg = DockerConfig::default();
        cfg.storage_driver = DockerStorageDriver::FuseOverlayfs;
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains(r#""storage_driver":"fuse-overlayfs""#));
    }

    #[test]
    fn test_sandbox_config_docker_absent_is_byte_identical() {
        let config = SandboxConfig::builder().name("t").image("alpine").build();
        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("docker"));
    }

    #[test]
    fn test_sandbox_config_docker_roundtrip() {
        let mut config = SandboxConfig::builder().name("t").image("alpine").build();
        config.docker = Some(DockerConfig {
            enabled: true,
            runtime: DockerRuntime::Containerd,
            storage_driver: DockerStorageDriver::FuseOverlayfs,
            data_size_bytes: 10 * 1024 * 1024 * 1024,
            network: DockerNetwork::None,
            daemon_json: HashMap::new(),
            preload_images: vec!["busybox".to_string()],
            rootless: true,
            socket: "/run/docker.sock".to_string(),
        });
        let json = serde_json::to_string(&config).unwrap();
        let back: SandboxConfig = serde_json::from_str(&json).unwrap();
        let docker = back.docker.unwrap();
        assert!(docker.enabled);
        assert_eq!(docker.runtime, DockerRuntime::Containerd);
        assert_eq!(docker.storage_driver, DockerStorageDriver::FuseOverlayfs);
        assert_eq!(docker.data_size_bytes, 10 * 1024 * 1024 * 1024);
        assert_eq!(docker.network, DockerNetwork::None);
        assert_eq!(docker.preload_images, vec!["busybox".to_string()]);
        assert!(docker.rootless);
        assert_eq!(docker.socket, "/run/docker.sock");
    }

    #[test]
    fn test_docker_validate_disabled_never_fails() {
        let cfg = DockerConfig {
            enabled: false,
            rootless: true,
            daemon_json: HashMap::from([(
                "iptables".to_string(),
                serde_json::from_str::<serde_json::Value>(r#"true"#).unwrap(),
            )]),
            ..DockerConfig::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_docker_validate_rejects_rootless_when_enabled() {
        let cfg = DockerConfig {
            enabled: true,
            rootless: true,
            ..DockerConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("rootless"));
    }

    #[test]
    fn test_docker_validate_allows_clean_config() {
        let cfg = DockerConfig {
            enabled: true,
            daemon_json: HashMap::from([
                (
                    "log-driver".to_string(),
                    serde_json::from_str::<serde_json::Value>(r#""json-file""#).unwrap(),
                ),
                (
                    "log-level".to_string(),
                    serde_json::from_str::<serde_json::Value>(r#""debug""#).unwrap(),
                ),
                (
                    "log-opts".to_string(),
                    serde_json::from_str::<serde_json::Value>(r#"{"max-size":"10m"}"#).unwrap(),
                ),
            ]),
            preload_images: vec!["alpine".to_string(), "ghcr.io/x/y:latest".to_string()],
            ..DockerConfig::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_docker_validate_rejects_forbidden_daemon_key() {
        let cfg = DockerConfig {
            enabled: true,
            daemon_json: HashMap::from([(
                "iptables".to_string(),
                serde_json::from_str::<serde_json::Value>(r#"true"#).unwrap(),
            )]),
            ..DockerConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("iptables"));
    }

    #[test]
    fn test_docker_validate_rejects_unknown_daemon_key() {
        let cfg = DockerConfig {
            enabled: true,
            daemon_json: HashMap::from([(
                "made-up-key".to_string(),
                serde_json::from_str::<serde_json::Value>(r#""x""#).unwrap(),
            )]),
            ..DockerConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("not allowed"));
    }

    #[test]
    fn test_docker_validate_rejects_credential_preload() {
        let cfg = DockerConfig {
            enabled: true,
            preload_images: vec!["https://u:p@host/img".to_string()],
            ..DockerConfig::default()
        };
        assert!(cfg.validate().is_err());

        let cfg2 = DockerConfig {
            enabled: true,
            preload_images: vec!["u:p@ghcr.io/img".to_string()],
            ..DockerConfig::default()
        };
        assert!(cfg2.validate().is_err());
    }

    #[test]
    fn test_docker_validate_rejects_empty_preload() {
        let cfg = DockerConfig {
            enabled: true,
            preload_images: vec!["".to_string()],
            ..DockerConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("preload"));
    }

    #[test]
    fn test_docker_validate_rejects_mirror_userinfo() {
        let cfg = DockerConfig {
            enabled: true,
            daemon_json: HashMap::from([(
                "registry-mirrors".to_string(),
                serde_json::from_str::<serde_json::Value>(r#"["https://user:pass@mirror"]"#)
                    .unwrap(),
            )]),
            ..DockerConfig::default()
        };
        assert!(cfg.validate().is_err());
    }
}

/// Default OCI registry for agent images.
pub const DEFAULT_AGENTS_REGISTRY: &str = "ghcr.io/nanosandboxai/agents-registry";

/// Known code-agent image names hosted in the agents registry.
const AGENT_IMAGES: &[&str] = &[
    "base",
    "claude",
    "claude-docker",
    "codex",
    "cursor",
    "goose",
    "legacy",
];

/// Normalize an image reference: known agent names get the agents-registry
/// prefix, other bare names get `docker.io/library/`, fully-qualified refs are
/// returned unchanged.
pub fn normalize_image(image: &str) -> String {
    if image.is_empty() {
        return image.to_string();
    }
    let name_part = image.split(':').next().unwrap_or(image);
    let has_registry =
        name_part.contains('/') || name_part.contains('.') || image.starts_with("localhost");
    if has_registry {
        image.to_string()
    } else {
        let (name, tag) = if let Some((n, t)) = image.split_once(':') {
            (n, t)
        } else {
            (image, "latest")
        };
        if AGENT_IMAGES.contains(&name) {
            format!("{}/{}:{}", DEFAULT_AGENTS_REGISTRY, name, tag)
        } else {
            format!("docker.io/library/{}:{}", name, tag)
        }
    }
}
