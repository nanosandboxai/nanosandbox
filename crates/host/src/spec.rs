//! FFI-free conversion of a protocol `SandboxSpec` into a runtime sandbox config.

use std::collections::HashMap;

use nanosb_protocol::{MountSpec, SandboxSpec};
use runtime_config::{
    Mount, NetworkConfig, NetworkMode, NetworkScope, PortMapping, RuntimeMode, SandboxConfig,
};

/// Typed error for mount conversion. `Unsupported` maps to the protocol
/// `unsupported` code at the verb boundary.
#[derive(Debug)]
pub enum MountError {
    Unsupported(String),
}

impl std::fmt::Display for MountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MountError::Unsupported(msg) => write!(f, "unsupported mount: {msg}"),
        }
    }
}

impl std::error::Error for MountError {}

/// Convert a protocol `MountSpec` into a runtime `Mount`.
pub fn mount_spec_to_mount(spec: &MountSpec) -> Result<Mount, MountError> {
    let mut mount = if let Some(format) = &spec.disk_image {
        let mut m = Mount::virtiofs(std::path::PathBuf::from(&spec.host_path), &spec.guest_path);
        m.disk_image = Some(format.clone());
        m.fstype = spec.fstype.clone();
        m
    } else if spec.tmpfs || spec.mount_type == nanosb_protocol::MountType::Tmpfs {
        Mount::tmpfs(&spec.guest_path)
    } else {
        match spec.mount_type {
            nanosb_protocol::MountType::Virtiofs => {
                Mount::virtiofs(std::path::PathBuf::from(&spec.host_path), &spec.guest_path)
            }
            nanosb_protocol::MountType::Bind => {
                Mount::bind(std::path::PathBuf::from(&spec.host_path), &spec.guest_path)
            }
            nanosb_protocol::MountType::Tmpfs => Mount::tmpfs(&spec.guest_path),
        }
    };
    mount.readonly = spec.readonly;
    mount.file = spec.file;
    mount.options = spec.options.clone();
    mount.uid = spec.uid;
    mount.gid = spec.gid;
    if mount.mount_type != runtime_config::MountType::Tmpfs
        && mount.disk_image.is_none()
        && !mount.file
        && !mount.host_path.exists()
    {
        return Err(MountError::Unsupported(format!(
            "mount host path '{}' does not exist",
            mount.host_path.display()
        )));
    }
    if (mount.file || mount.disk_image.is_some())
        && mount.fstype.is_none()
        && mount.disk_image.is_some()
    {
        return Err(MountError::Unsupported(
            "disk-image mount requires a fstype".to_string(),
        ));
    }
    Ok(mount)
}

/// Build an FFI-free runtime sandbox config from a protocol spec (without spawning).
pub fn spec_to_config(spec: &SandboxSpec) -> anyhow::Result<SandboxConfig> {
    let image = runtime_config::normalize_image(&spec.image);
    let mut config = SandboxConfig::builder()
        .name(spec.name.clone().unwrap_or_default())
        .image(&image)
        .cpus(spec.cpus.unwrap_or(2))
        .memory_mb(spec.memory_mb.unwrap_or(4096))
        .timeout_secs(spec.timeout_secs.unwrap_or(3600))
        .runtime_mode(RuntimeMode::Next)
        .build();

    if let Some(w) = &spec.workdir {
        config.workdir = w.clone();
    }
    if let Some(u) = &spec.user {
        config.user = Some(u.clone());
    }
    if let Some(h) = &spec.home {
        config.home = Some(h.clone());
    }
    config.env = spec.env.clone();
    config.labels = spec.labels.clone();
    config.ephemeral = spec.ephemeral;

    for m in &spec.mounts {
        let mount = mount_spec_to_mount(m)?;
        config.mounts.push(mount);
    }

    config.network = NetworkConfig {
        enabled: spec.network.enabled,
        mode: match spec.network.mode.as_deref() {
            Some("none") => NetworkMode::None,
            Some("bridge") => NetworkMode::Bridge,
            Some("tsi") | None => NetworkMode::Tsi,
            Some(other) => anyhow::bail!("unknown network mode '{other}'"),
        },
        scope: match spec.network.scope.as_deref() {
            Some("none") => NetworkScope::None,
            Some("group") => NetworkScope::Group,
            Some("public") => NetworkScope::Public,
            Some("any") | None => NetworkScope::Any,
            Some(other) => anyhow::bail!("unknown network scope '{other}'"),
        },
        port_mappings: spec
            .network
            .publish
            .iter()
            .map(|p| PortMapping {
                host_port: p.host_port,
                container_port: p.container_port,
                protocol: p.protocol.clone(),
                bind_address: p.bind_address.clone(),
            })
            .collect(),
        dns: spec.network.dns.clone(),
        dns_zones: spec.network.dns_zones.clone(),
        policy: spec.network.policy.clone(),
    };

    if let Some(cmd) = &spec.command {
        config.command = Some(cmd.clone());
        config.command_args = spec.command_args.clone();
    }

    if let Some(h) = &spec.hostname {
        config.hostname = Some(h.clone());
    }
    config.scripts = spec.scripts.clone();
    config.stat_virtualization = spec.stat_virtualization.map(|s| {
        use nanosb_protocol::StatVirtualization as S;
        match s {
            S::Strict => "strict",
            S::Relaxed => "relaxed",
            S::Off => "off",
        }
        .to_string()
    });
    config.security_profile = match spec.security_profile {
        nanosb_protocol::SecurityProfile::Default => "default",
        nanosb_protocol::SecurityProfile::Restricted => "restricted",
    }
    .to_string();
    config.rlimits = spec.rlimits.iter().map(format_rlimit).collect();
    config.registry = spec
        .registry
        .iter()
        .map(|r| runtime_config::RegistryConfig {
            host: r.host.clone(),
            insecure: r.insecure,
            skip_tls_verify: r.skip_tls_verify,
        })
        .collect();
    if let Some(docker) = &spec.docker {
        config.docker = Some(runtime_config::DockerConfig {
            enabled: docker.enabled,
            runtime: match docker.runtime {
                nanosb_protocol::DockerRuntime::Dockerd => runtime_config::DockerRuntime::Dockerd,
                nanosb_protocol::DockerRuntime::Podman => runtime_config::DockerRuntime::Podman,
                nanosb_protocol::DockerRuntime::Containerd => {
                    runtime_config::DockerRuntime::Containerd
                }
            },
            storage_driver: match docker.storage_driver {
                nanosb_protocol::DockerStorageDriver::Overlay2 => {
                    runtime_config::DockerStorageDriver::Overlay2
                }
                nanosb_protocol::DockerStorageDriver::Vfs => {
                    runtime_config::DockerStorageDriver::Vfs
                }
                nanosb_protocol::DockerStorageDriver::FuseOverlayfs => {
                    runtime_config::DockerStorageDriver::FuseOverlayfs
                }
            },
            data_size_bytes: docker.data_size_bytes,
            network: match docker.network {
                nanosb_protocol::DockerNetwork::Host => runtime_config::DockerNetwork::Host,
                nanosb_protocol::DockerNetwork::None => runtime_config::DockerNetwork::None,
                nanosb_protocol::DockerNetwork::Bridge => runtime_config::DockerNetwork::Bridge,
            },
            daemon_json: docker.daemon_json.clone(),
            preload_images: docker.preload_images.clone(),
            rootless: docker.rootless,
            socket: docker.socket.clone(),
        });
    }
    if let Some(rootfs) = &spec.rootfs {
        use nanosb_protocol::RootfsKind as K;
        config.rootfs = Some(runtime_config::RootfsConfig {
            source: match rootfs.kind {
                K::Oci => runtime_config::RootfsSource::Oci,
                K::LocalDir => runtime_config::RootfsSource::LocalDir,
                K::DiskImage => runtime_config::RootfsSource::DiskImage,
            },
            path: rootfs.path.clone(),
            fstype: rootfs.fstype.clone(),
            format: rootfs.format.clone(),
            read_only: rootfs.read_only,
        });
    }
    config.patches = spec.patches.iter().map(rootfs_patch_to_config).collect();

    let create_secrets: HashMap<String, String> = spec
        .secrets
        .iter()
        .filter_map(|s| s.value.clone().map(|v| (s.key.clone(), v)))
        .collect();
    for (env_key, secret_name) in &spec.secret_env {
        let value = create_secrets
            .get(secret_name)
            .cloned()
            .or_else(|| std::env::var(secret_name).ok())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "secret '{secret_name}' referenced by secret_env '{env_key}' is not defined"
                )
            })?;
        config.env.insert(env_key.clone(), value);
    }

    Ok(config)
}

fn format_rlimit(r: &nanosb_protocol::Rlimit) -> String {
    let soft = r
        .soft
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let hard = r
        .hard
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    format!("{}={}:{}", r.resource.to_uppercase(), soft, hard)
}

fn rootfs_patch_to_config(p: &nanosb_protocol::RootfsPatch) -> runtime_config::RootfsPatchConfig {
    use nanosb_protocol::RootfsPatch as P;
    use runtime_config::RootfsPatchConfig as C;
    match p {
        P::Mkdir { path, mode } => C::Mkdir {
            path: path.clone(),
            mode: *mode,
        },
        P::Text {
            path,
            content,
            mode,
        } => C::Text {
            path: path.clone(),
            content: content.clone(),
            mode: *mode,
        },
        P::CopyFile { from, to, mode } => C::CopyFile {
            from: from.clone(),
            to: to.clone(),
            mode: *mode,
        },
        P::CopyDir { from, to } => C::CopyDir {
            from: from.clone(),
            to: to.clone(),
        },
        P::Remove { path, recursive } => C::Remove {
            path: path.clone(),
            recursive: *recursive,
        },
    }
}
