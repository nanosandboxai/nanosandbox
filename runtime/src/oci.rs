//! OCI Runtime Specification generation
//!
//! Generates config.json files conforming to the OCI Runtime Specification
//! for use with crun/libkrun.

use crate::config::{Mount, MountType, NetworkMode, SandboxConfig};
use serde_json::json;
use std::path::Path;

/// OCI Runtime Specification version
pub const OCI_VERSION: &str = "1.0.2";

/// Generate an OCI runtime config.json
pub fn generate_config(config: &SandboxConfig, rootfs_path: &Path) -> serde_json::Value {
    let env: Vec<String> = config
        .env
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .chain(default_env())
        .collect();

    let mounts = generate_mounts(&config.mounts);

    json!({
        "ociVersion": OCI_VERSION,
        "process": {
            "terminal": false,
            "user": {
                "uid": 0,
                "gid": 0
            },
            "args": ["/bin/sh"],
            "env": env,
            "cwd": &config.workdir,
            "capabilities": generate_capabilities(),
            "rlimits": [
                {
                    "type": "RLIMIT_NOFILE",
                    "hard": 1024,
                    "soft": 1024
                }
            ],
            "noNewPrivileges": true
        },
        "root": {
            "path": rootfs_path.to_str().unwrap_or("rootfs"),
            "readonly": false
        },
        "hostname": config.name.chars().take(64).collect::<String>(),
        "mounts": mounts,
        "linux": generate_linux_config(config)
    })
}

/// Generate default environment variables
fn default_env() -> Vec<String> {
    vec![
        "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
        "TERM=xterm".to_string(),
        "HOME=/root".to_string(),
    ]
}

/// Generate mount specifications
fn generate_mounts(user_mounts: &[Mount]) -> Vec<serde_json::Value> {
    let mut mounts = vec![
        // Essential filesystem mounts
        json!({
            "destination": "/proc",
            "type": "proc",
            "source": "proc",
            "options": ["nosuid", "noexec", "nodev"]
        }),
        json!({
            "destination": "/dev",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "strictatime", "mode=755", "size=65536k"]
        }),
        json!({
            "destination": "/dev/pts",
            "type": "devpts",
            "source": "devpts",
            "options": ["nosuid", "noexec", "newinstance", "ptmxmode=0666", "mode=0620"]
        }),
        json!({
            "destination": "/dev/shm",
            "type": "tmpfs",
            "source": "shm",
            "options": ["nosuid", "noexec", "nodev", "mode=1777", "size=65536k"]
        }),
        json!({
            "destination": "/dev/mqueue",
            "type": "mqueue",
            "source": "mqueue",
            "options": ["nosuid", "noexec", "nodev"]
        }),
        json!({
            "destination": "/sys",
            "type": "sysfs",
            "source": "sysfs",
            "options": ["nosuid", "noexec", "nodev", "ro"]
        }),
        json!({
            "destination": "/sys/fs/cgroup",
            "type": "cgroup",
            "source": "cgroup",
            "options": ["nosuid", "noexec", "nodev", "relatime", "ro"]
        }),
        json!({
            "destination": "/tmp",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "nodev", "mode=1777"]
        }),
    ];

    // Add user-defined mounts
    for mount in user_mounts {
        match mount.mount_type {
            MountType::Bind => {
                let mut options = vec!["rbind".to_string()];
                if mount.readonly {
                    options.push("ro".to_string());
                } else {
                    options.push("rw".to_string());
                }

                mounts.push(json!({
                    "destination": &mount.container_path,
                    "type": "bind",
                    "source": mount.host_path.to_str().unwrap_or(""),
                    "options": options
                }));
            }
            MountType::VirtioFs => {
                // virtio-fs mounts use a tag-based source
                // The tag is typically the host path, but can be customized
                let tag = mount
                    .host_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("virtiofs");

                let mut options = vec![];
                if mount.readonly {
                    options.push("ro".to_string());
                } else {
                    options.push("rw".to_string());
                }

                mounts.push(json!({
                    "destination": &mount.container_path,
                    "type": "virtiofs",
                    "source": tag,
                    "options": options
                }));
            }
        }
    }

    mounts
}

/// Generate Linux-specific configuration
fn generate_linux_config(config: &SandboxConfig) -> serde_json::Value {
    let memory_limit = (config.memory_mb as i64) * 1024 * 1024;
    
    // CPU quota: microseconds per period that the container can use
    // 100000 microseconds = 100ms = 1 CPU core at 100%
    // So for N cores, we allow N * 100000 microseconds per period
    let cpu_quota = (config.cpus as i64) * 100000;
    let cpu_period = 100000_i64; // 100ms period (standard)
    
    // PIDs limit to prevent fork bombs
    let pids_limit = 256_i64;

    json!({
        "resources": {
            "memory": {
                "limit": memory_limit,
                // Set swap to same as memory to effectively disable swap
                "swap": memory_limit,
                // Disable OOM killer - let the process fail instead
                "disableOOMKiller": false
            },
            "cpu": {
                // Relative weight for CPU time sharing
                "shares": config.cpus * 1024,
                // Hard limit: quota microseconds per period
                "quota": cpu_quota,
                "period": cpu_period
            },
            "pids": {
                // Limit number of processes to prevent fork bombs
                "limit": pids_limit
            }
        },
        "namespaces": generate_namespaces(&config.network.mode),
        "maskedPaths": [
            "/proc/acpi",
            "/proc/asound",
            "/proc/kcore",
            "/proc/keys",
            "/proc/latency_stats",
            "/proc/timer_list",
            "/proc/timer_stats",
            "/proc/sched_debug",
            "/sys/firmware",
            "/proc/scsi"
        ],
        "readonlyPaths": [
            "/proc/bus",
            "/proc/fs",
            "/proc/irq",
            "/proc/sys",
            "/proc/sysrq-trigger"
        ]
    })
}

/// Generate namespace configuration
fn generate_namespaces(network_mode: &NetworkMode) -> Vec<serde_json::Value> {
    let mut namespaces = vec![
        json!({"type": "pid"}),
        json!({"type": "ipc"}),
        json!({"type": "uts"}),
        json!({"type": "mount"}),
        json!({"type": "cgroup"}),
    ];

    // Add network namespace based on mode
    match network_mode {
        NetworkMode::None => {
            // Full network isolation - creates isolated network namespace
            // with no connectivity
            namespaces.push(json!({"type": "network"}));
        }
        NetworkMode::Tsi => {
            // TSI (Transparent Socket Impersonation) mode
            // Does NOT create a network namespace - sockets are intercepted
            // by libkrun and proxied through the host network stack
            // This allows outbound connections without virtual interfaces
        }
        NetworkMode::Bridge => {
            // Bridge mode - creates network namespace with virtual interface
            // Requires passt/gvproxy to be set up for connectivity
            namespaces.push(json!({"type": "network"}));
        }
    }

    namespaces
}

/// Generate capabilities configuration
fn generate_capabilities() -> serde_json::Value {
    let caps = vec![
        "CAP_CHOWN",
        "CAP_DAC_OVERRIDE",
        "CAP_FSETID",
        "CAP_FOWNER",
        "CAP_MKNOD",
        "CAP_NET_RAW",
        "CAP_SETGID",
        "CAP_SETUID",
        "CAP_SETFCAP",
        "CAP_SETPCAP",
        "CAP_NET_BIND_SERVICE",
        "CAP_SYS_CHROOT",
        "CAP_KILL",
        "CAP_AUDIT_WRITE",
    ];

    json!({
        "bounding": caps,
        "effective": caps,
        "inheritable": caps,
        "permitted": caps,
        "ambient": caps
    })
}

/// OCI bundle structure
#[derive(Debug)]
pub struct OciBundle {
    /// Path to the bundle directory
    pub path: std::path::PathBuf,
    /// Path to config.json
    pub config_path: std::path::PathBuf,
    /// Path to rootfs
    pub rootfs_path: std::path::PathBuf,
}

impl OciBundle {
    /// Create a new OCI bundle directory structure
    pub fn create(base_dir: &Path, sandbox_id: &str) -> crate::error::Result<Self> {
        let bundle_path = base_dir.join(sandbox_id);
        let rootfs_path = bundle_path.join("rootfs");
        let config_path = bundle_path.join("config.json");

        std::fs::create_dir_all(&rootfs_path)?;

        Ok(Self {
            path: bundle_path,
            config_path,
            rootfs_path,
        })
    }

    /// Write the OCI config to the bundle
    pub fn write_config(&self, config: &serde_json::Value) -> crate::error::Result<()> {
        let content = serde_json::to_string_pretty(config)?;
        std::fs::write(&self.config_path, content)?;
        Ok(())
    }

    /// Get the bundle path as a string
    pub fn path_str(&self) -> &str {
        self.path.to_str().unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SandboxConfig;

    #[test]
    fn test_generate_config() {
        let config = SandboxConfig::builder()
            .name("test-sandbox")
            .image("alpine:latest")
            .cpus(2)
            .memory_mb(512)
            .build();

        let oci_config = generate_config(&config, Path::new("rootfs"));

        assert_eq!(oci_config["ociVersion"], OCI_VERSION);
        assert_eq!(oci_config["process"]["cwd"], "/workspace");
        assert_eq!(oci_config["root"]["path"], "rootfs");
    }

    #[test]
    fn test_generate_mounts() {
        let mounts = generate_mounts(&[]);
        assert!(!mounts.is_empty());

        // Should have proc, dev, sys, etc.
        let destinations: Vec<&str> = mounts
            .iter()
            .filter_map(|m| m["destination"].as_str())
            .collect();

        assert!(destinations.contains(&"/proc"));
        assert!(destinations.contains(&"/dev"));
        assert!(destinations.contains(&"/sys"));
    }
}
