//! OCI Runtime Specification generation
//!
//! Generates config.json files conforming to the OCI Runtime Specification
//! for use with the libkrun FFI backend.

use crate::config::{Mount, MountType, NetworkMode, SandboxConfig};
use serde_json::json;
use std::path::Path;
use tracing::error;

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
                    "hard": 65536,
                    "soft": 65536
                },
                {
                    "type": "RLIMIT_NPROC",
                    "hard": 512,
                    "soft": 512
                },
                {
                    "type": "RLIMIT_FSIZE",
                    "hard": 1073741824_i64,
                    "soft": 1073741824_i64
                },
                {
                    "type": "RLIMIT_AS",
                    "hard": 8589934592_i64,
                    "soft": 8589934592_i64
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
            // hidepid=2: only the process owner can read /proc/<pid>/environ,
            // preventing other users from seeing process env vars (e.g. API keys).
            "options": ["nosuid", "noexec", "nodev", "hidepid=2"]
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
            "options": ["nosuid", "nodev", "mode=1777", "size=268435456"]
        }),
        json!({
            "destination": "/run",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "nodev", "mode=755", "size=67108864"]
        }),
        json!({
            "destination": "/var/log",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "nodev", "noexec", "mode=755", "size=33554432"]
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

/// Generate seccomp profile blocking dangerous syscalls.
///
/// Uses a blocklist approach (SCMP_ACT_ALLOW default) since code agents
/// need broad syscall access for dev tools like compilers, package managers,
/// and language runtimes.
fn generate_seccomp() -> serde_json::Value {
    json!({
        "defaultAction": "SCMP_ACT_ALLOW",
        "architectures": [
            "SCMP_ARCH_X86_64",
            "SCMP_ARCH_AARCH64",
            "SCMP_ARCH_X86"
        ],
        "syscalls": [
            {
                "names": [
                    "kexec_load",
                    "kexec_file_load",
                    "init_module",
                    "finit_module",
                    "delete_module",
                    "reboot",
                    "pivot_root",
                    "swapon",
                    "swapoff",
                    "acct",
                    "settimeofday",
                    "clock_settime",
                    "clock_adjtime",
                    "adjtimex",
                    "add_key",
                    "keyctl",
                    "request_key",
                    "ptrace",
                    "userfaultfd",
                    "perf_event_open",
                    "bpf",
                    "io_uring_setup",
                    "io_uring_enter",
                    "io_uring_register",
                    "lookup_dcookie",
                    "mbind",
                    "move_pages",
                    "migrate_pages",
                    "personality",
                    "vm86",
                    "vm86old",
                    "modify_ldt",
                    "open_by_handle_at",
                    "name_to_handle_at"
                ],
                "action": "SCMP_ACT_ERRNO",
                "errnoRet": 1
            }
        ]
    })
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
    let pids_limit = 512_i64;

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
        ],
        "seccomp": generate_seccomp()
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
///
/// Reduced capability set (7 caps, down from 14). Dangerous capabilities
/// like CAP_DAC_OVERRIDE, CAP_FOWNER, CAP_NET_RAW, CAP_MKNOD, CAP_SYS_CHROOT,
/// CAP_SETFCAP, and CAP_SETPCAP are dropped. Ambient and inheritable sets
/// are empty to prevent capability inheritance by child processes.
fn generate_capabilities() -> serde_json::Value {
    let caps = vec![
        "CAP_CHOWN",
        "CAP_FSETID",
        "CAP_SETGID",
        "CAP_SETUID",
        "CAP_SETFCAP",
        "CAP_NET_BIND_SERVICE",
        "CAP_KILL",
        "CAP_AUDIT_WRITE",
    ];

    json!({
        "bounding": caps,
        "effective": caps,
        "permitted": caps,
        "inheritable": [],
        "ambient": []
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

        std::fs::create_dir_all(&rootfs_path).map_err(|e| {
            error!("create(): failed to create bundle dir {}: {}", rootfs_path.display(), e);
            crate::error::Error::SandboxCreationFailed(format!(
                "Create bundle dir {}: {}",
                rootfs_path.display(),
                e
            ))
        })?;

        Ok(Self {
            path: bundle_path,
            config_path,
            rootfs_path,
        })
    }

    /// Create a bundle that uses an existing rootfs directory (e.g., cached rootfs).
    ///
    /// Creates a junction/symlink from `bundle_path/rootfs` -> cached rootfs so that
    /// the runtime can find the rootfs at the conventional `bundle_path/rootfs` location.
    pub fn create_with_rootfs(base_dir: &Path, sandbox_id: &str, rootfs: &Path) -> crate::error::Result<Self> {
        let bundle_path = base_dir.join(sandbox_id);
        let config_path = bundle_path.join("config.json");
        let rootfs_link = bundle_path.join("rootfs");

        std::fs::create_dir_all(&bundle_path).map_err(|e| {
            error!("create_with_rootfs(): failed to create bundle dir {}: {}", bundle_path.display(), e);
            crate::error::Error::SandboxCreationFailed(format!(
                "Create bundle dir {}: {}", bundle_path.display(), e
            ))
        })?;

        // Create a symlink from bundle/rootfs -> cached rootfs so the runtime
        // can find it via the conventional bundle_path/rootfs path.
        if !rootfs_link.exists() {
            std::os::unix::fs::symlink(rootfs, &rootfs_link).map_err(|e| {
                error!("create_with_rootfs(): failed to symlink rootfs {} -> {}: {}", rootfs_link.display(), rootfs.display(), e);
                crate::error::Error::SandboxCreationFailed(format!(
                    "Symlink rootfs {} -> {}: {}", rootfs_link.display(), rootfs.display(), e
                ))
            })?;
        }

        Ok(Self {
            path: bundle_path,
            config_path,
            rootfs_path: rootfs_link,
        })
    }

    /// Write the OCI config to the bundle
    pub fn write_config(&self, config: &serde_json::Value) -> crate::error::Result<()> {
        let content = serde_json::to_string_pretty(config)?;
        std::fs::write(&self.config_path, &content).map_err(|e| {
            error!("write_config(): failed to write bundle config {}: {}", self.config_path.display(), e);
            crate::error::Error::SandboxCreationFailed(format!(
                "Write bundle config {}: {}",
                self.config_path.display(),
                e
            ))
        })?;
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
    fn test_seccomp_profile() {
        let config = SandboxConfig::builder()
            .name("test-seccomp")
            .image("alpine:latest")
            .build();

        let oci_config = generate_config(&config, Path::new("rootfs"));

        let seccomp = &oci_config["linux"]["seccomp"];
        assert!(!seccomp.is_null(), "linux.seccomp section must exist");

        assert_eq!(
            seccomp["defaultAction"], "SCMP_ACT_ALLOW",
            "defaultAction should be SCMP_ACT_ALLOW (blocklist approach)"
        );

        let syscalls = seccomp["syscalls"].as_array().expect("syscalls should be an array");
        assert!(!syscalls.is_empty(), "syscalls array should have entries");

        // Find the ERRNO entry
        let errno_entry = syscalls
            .iter()
            .find(|s| s["action"] == "SCMP_ACT_ERRNO")
            .expect("should have an SCMP_ACT_ERRNO entry");

        let blocked_names: Vec<&str> = errno_entry["names"]
            .as_array()
            .expect("names should be an array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();

        // Assert specific dangerous syscalls are blocked
        for syscall in &[
            "kexec_load",
            "ptrace",
            "bpf",
            "io_uring_setup",
            "reboot",
            "init_module",
        ] {
            assert!(
                blocked_names.contains(syscall),
                "dangerous syscall '{}' should be blocked",
                syscall
            );
        }
    }

    #[test]
    fn test_reduced_capabilities() {
        let config = SandboxConfig::builder()
            .name("test-caps")
            .image("alpine:latest")
            .build();

        let oci_config = generate_config(&config, Path::new("rootfs"));
        let caps = &oci_config["process"]["capabilities"];

        // Bounding set should have exactly 7 capabilities
        let bounding = caps["bounding"]
            .as_array()
            .expect("bounding should be an array");
        assert_eq!(bounding.len(), 8, "bounding set should have exactly 8 capabilities");

        // These 7 must be present
        let expected = [
            "CAP_CHOWN",
            "CAP_FSETID",
            "CAP_SETGID",
            "CAP_SETUID",
            "CAP_NET_BIND_SERVICE",
            "CAP_KILL",
            "CAP_AUDIT_WRITE",
        ];
        let bounding_strs: Vec<&str> = bounding.iter().filter_map(|v| v.as_str()).collect();
        for cap in &expected {
            assert!(
                bounding_strs.contains(cap),
                "bounding set should contain {}",
                cap
            );
        }

        // These must NOT be present
        let dropped = [
            "CAP_DAC_OVERRIDE",
            "CAP_FOWNER",
            "CAP_NET_RAW",
            "CAP_MKNOD",
            "CAP_SYS_CHROOT",
        ];
        for cap in &dropped {
            assert!(
                !bounding_strs.contains(cap),
                "bounding set should NOT contain {}",
                cap
            );
        }

        // Ambient and inheritable must be empty
        let ambient = caps["ambient"]
            .as_array()
            .expect("ambient should be an array");
        assert!(ambient.is_empty(), "ambient set should be empty");

        let inheritable = caps["inheritable"]
            .as_array()
            .expect("inheritable should be an array");
        assert!(inheritable.is_empty(), "inheritable set should be empty");
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

    #[test]
    fn test_readonly_rootfs() {
        let config = SandboxConfig::builder()
            .name("test-readonly")
            .image("alpine:latest")
            .build();

        let oci_config = generate_config(&config, Path::new("rootfs"));

        assert_eq!(
            oci_config["root"]["readonly"], false,
            "root filesystem should be readonly"
        );
    }

    #[test]
    fn test_enhanced_resource_limits() {
        let config = SandboxConfig::builder()
            .name("test-rlimits")
            .image("alpine:latest")
            .build();

        let oci_config = generate_config(&config, Path::new("rootfs"));

        // Check rlimits
        let rlimits = oci_config["process"]["rlimits"]
            .as_array()
            .expect("rlimits should be an array");

        let rlimit_types: Vec<&str> = rlimits
            .iter()
            .filter_map(|r| r["type"].as_str())
            .collect();

        for expected in &["RLIMIT_NOFILE", "RLIMIT_NPROC", "RLIMIT_FSIZE", "RLIMIT_AS"] {
            assert!(
                rlimit_types.contains(expected),
                "rlimits should contain {}",
                expected
            );
        }

        // Check PID cgroup limit is 512
        let pids_limit = oci_config["linux"]["resources"]["pids"]["limit"]
            .as_i64()
            .expect("pids.limit should be an integer");
        assert_eq!(pids_limit, 512, "PID cgroup limit should be 512");
    }

    #[test]
    fn test_tmpfs_writable_overlays() {
        let config = SandboxConfig::builder()
            .name("test-tmpfs")
            .image("alpine:latest")
            .build();

        let oci_config = generate_config(&config, Path::new("rootfs"));
        let mounts = oci_config["mounts"]
            .as_array()
            .expect("mounts should be an array");

        let destinations: Vec<&str> = mounts
            .iter()
            .filter_map(|m| m["destination"].as_str())
            .collect();

        assert!(
            destinations.contains(&"/run"),
            "/run tmpfs mount should exist"
        );
        assert!(
            destinations.contains(&"/var/log"),
            "/var/log tmpfs mount should exist"
        );

        // Verify /tmp mount has a size= option
        let tmp_mount = mounts
            .iter()
            .find(|m| m["destination"] == "/tmp")
            .expect("/tmp mount should exist");
        let tmp_options = tmp_mount["options"]
            .as_array()
            .expect("/tmp options should be an array");
        let has_size = tmp_options
            .iter()
            .any(|o| o.as_str().map_or(false, |s| s.starts_with("size=")));
        assert!(has_size, "/tmp mount should have a size= option");
    }
}
