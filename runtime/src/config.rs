//! Sandbox configuration

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Configuration for creating a sandbox
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Unique name for the sandbox
    pub name: String,

    /// OCI image reference (e.g., "ghcr.io/devdone-labs/dd-agents:latest")
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
}

fn default_cpus() -> u32 {
    2
}

fn default_memory() -> u32 {
    4096
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

    /// Add a mount point
    pub fn mount(mut self, host: impl Into<PathBuf>, container: impl Into<String>) -> Self {
        self.config.mounts.push(Mount {
            host_path: host.into(),
            container_path: container.into(),
            readonly: false,
        });
        self
    }

    /// Add a readonly mount point
    pub fn mount_readonly(
        mut self,
        host: impl Into<PathBuf>,
        container: impl Into<String>,
    ) -> Self {
        self.config.mounts.push(Mount {
            host_path: host.into(),
            container_path: container.into(),
            readonly: true,
        });
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

    /// Build the configuration
    pub fn build(self) -> SandboxConfig {
        self.config
    }
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
}

fn default_network_enabled() -> bool {
    true
}

fn default_network_mode() -> NetworkMode {
    NetworkMode::Tsi
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            enabled: default_network_enabled(),
            mode: default_network_mode(),
        }
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
