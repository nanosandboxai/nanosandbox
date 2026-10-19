//! Helper control plane: materialise a protocol `SandboxSpec` into a running
//! supervisor-managed sandbox. Shared by the `nanosb-runtime` daemon.

use std::collections::HashMap;

use nanosb_protocol::SandboxSpec;
use runtime::config::{Mount, SandboxConfig};

use crate::deploy;
pub use nanosb_host::spec::{mount_spec_to_mount, spec_to_config, MountError};

/// Append a runtime mount to a stopped sandbox's config so it applies on the
/// next boot. Used by `mount_add` for stopped sandboxes — live mounts are not
/// supported by the libkrun attach path (see `docs/decisions/volume-backings.md`).
pub fn persist_mount(sandbox_dir: &std::path::Path, mount: &Mount) -> anyhow::Result<()> {
    let path = sandbox_dir.join("config.json");
    let mut cfg: SandboxConfig = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    cfg.mounts.push(mount.clone());
    let json = serde_json::to_string_pretty(&cfg)?;
    std::fs::write(&path, json)?;
    Ok(())
}


/// Enforce `SandboxSpec.pull_policy` against the local image cache before the
/// supervisor pulls.
///
/// The supervisor always pulls, but `ImageManager::pull` is layer-cache-aware:
/// a fully cached image is not re-downloaded. So `Always` and `Missing` need no
/// gate here (a cached image under `Missing` is effectively a no-op pull), and
/// `Never` errors when the image is absent from the cache.
fn ensure_image_policy(spec: &SandboxSpec) -> anyhow::Result<()> {
    let mgr = runtime::ImageManager::with_default_cache()?;
    ensure_image_policy_with(spec, &mgr)
}

fn ensure_image_policy_with(spec: &SandboxSpec, mgr: &runtime::ImageManager) -> anyhow::Result<()> {
    use nanosb_protocol::PullPolicy;
    match spec.pull_policy {
        PullPolicy::Always | PullPolicy::Missing => Ok(()),
        PullPolicy::Never => {
            let image = sandbox::normalize_image(&spec.image);
            let rt = tokio::runtime::Runtime::new()?;
            let cached = rt.block_on(mgr.exists(&image))?;
            if cached {
                Ok(())
            } else {
                anyhow::bail!("image '{image}' is not cached and pull_policy is 'never'")
            }
        }
    }
}

/// Create, boot, and wait for a sandbox to reach the running state.
pub fn create_sandbox(spec: &SandboxSpec) -> anyhow::Result<()> {
    ensure_image_policy(spec)?;
    let sp = deploy::prepare_supervisor_spawn(spec)?;
    deploy::spawn_supervisor(
        &sp.name,
        &sp.config_json,
        &sp.extra_mounts_json,
        &sp.boot_env_json,
        &sp.origin_json,
        sp.timeout_secs,
    )?;
    deploy::wait_for_running(&sp.name, 60)?;
    deploy::wait_for_exec_channel(&sp.name, 20)?;
    Ok(())
}

/// Default label map helper (kept for SDK parity in future verbs).
pub fn empty_labels() -> HashMap<String, String> {
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanosb_protocol::{MountSpec, PortPublish, PullPolicy};
    use std::path::PathBuf;

    fn spec_with_publish(bind_address: Option<&str>) -> SandboxSpec {
        let mut spec = SandboxSpec::new("alpine");
        spec.name = Some("t".to_string());
        spec.network.publish.push(PortPublish {
            host_port: 8080,
            container_port: 80,
            protocol: "tcp".to_string(),
            bind_address: bind_address.map(String::from),
        });
        spec
    }

    #[test]
    fn spec_to_config_threads_bind_address() {
        let spec = spec_with_publish(Some("127.0.0.1"));
        let config = spec_to_config(&spec).unwrap();
        let pm = &config.network.port_mappings[0];
        assert_eq!(pm.host_port, 8080);
        assert_eq!(pm.container_port, 80);
        assert_eq!(pm.protocol, "tcp");
        assert_eq!(pm.bind_address.as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn spec_to_config_bind_address_none_when_unset() {
        let spec = spec_with_publish(None);
        let config = spec_to_config(&spec).unwrap();
        assert!(config.network.port_mappings[0].bind_address.is_none());
    }

    fn temp_cache() -> (tempfile::TempDir, runtime::ImageManager) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = runtime::ImageManager::new(dir.path().to_path_buf()).unwrap();
        (dir, mgr)
    }

    fn mark_cached(mgr: &runtime::ImageManager, image: &str) {
        let image = sandbox::normalize_image(image);
        let (name, tag) = image.split_once(':').unwrap_or((image.as_str(), "latest"));
        let (registry, repo) = match name.split_once('/') {
            Some((r, rest)) => (r, rest),
            None => ("docker.io", name),
        };
        let path = mgr
            .cache_dir()
            .join("manifests")
            .join(registry.replace('/', "_"))
            .join(repo.replace('/', "_"))
            .join(tag);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}").unwrap();
    }

    #[test]
    fn pull_policy_never_errors_when_image_absent() {
        let (_dir, mgr) = temp_cache();
        let mut spec = SandboxSpec::new("alpine");
        spec.pull_policy = PullPolicy::Never;
        let err = ensure_image_policy_with(&spec, &mgr).unwrap_err();
        assert!(err.to_string().contains("pull_policy is 'never'"), "{err}");
    }

    #[test]
    fn pull_policy_never_ok_when_image_cached() {
        let (_dir, mgr) = temp_cache();
        mark_cached(&mgr, "alpine");
        let mut spec = SandboxSpec::new("alpine");
        spec.pull_policy = PullPolicy::Never;
        ensure_image_policy_with(&spec, &mgr).unwrap();
    }

    #[test]
    fn pull_policy_always_and_missing_never_gate() {
        let (_dir, mgr) = temp_cache();
        let mut spec = SandboxSpec::new("alpine");
        spec.pull_policy = PullPolicy::Always;
        ensure_image_policy_with(&spec, &mgr).unwrap();
        spec.pull_policy = PullPolicy::Missing;
        ensure_image_policy_with(&spec, &mgr).unwrap();
    }

    // ── mount_spec_to_mount (decision-backed conversions) ───────────────

    fn mount_spec(mount_type: nanosb_protocol::MountType) -> MountSpec {
        let host = tempfile::tempdir().unwrap();
        let host_path = host.keep();
        MountSpec {
            host_path: host_path.to_string_lossy().to_string(),
            guest_path: "/mnt/data".to_string(),
            readonly: false,
            mount_type,
            file: false,
            tmpfs: false,
            disk_image: None,
            fstype: None,
            options: Vec::new(),
            uid: None,
            gid: None,
        }
    }

    #[test]
    fn bind_spec_converts_to_bind_mount() {
        let m = mount_spec_to_mount(&mount_spec(nanosb_protocol::MountType::Bind)).unwrap();
        assert_eq!(m.mount_type, runtime::config::MountType::Bind);
        assert_eq!(m.container_path, "/mnt/data");
        assert!(!m.readonly);
    }

    #[test]
    fn virtiofs_spec_converts_to_virtiofs_mount() {
        let m = mount_spec_to_mount(&mount_spec(nanosb_protocol::MountType::Virtiofs)).unwrap();
        assert_eq!(m.mount_type, runtime::config::MountType::VirtioFs);
    }

    #[test]
    fn readonly_is_threaded_through() {
        let mut s = mount_spec(nanosb_protocol::MountType::Bind);
        s.readonly = true;
        let m = mount_spec_to_mount(&s).unwrap();
        assert!(m.readonly);
    }

    #[test]
    fn tmpfs_spec_converts_to_tmpfs_mount() {
        let mut s = mount_spec(nanosb_protocol::MountType::Bind);
        s.tmpfs = true;
        let m = mount_spec_to_mount(&s).unwrap();
        assert_eq!(m.mount_type, runtime::config::MountType::Tmpfs);
        let s = mount_spec(nanosb_protocol::MountType::Tmpfs);
        assert_eq!(
            mount_spec_to_mount(&s).unwrap().mount_type,
            runtime::config::MountType::Tmpfs
        );
    }

    #[test]
    fn disk_image_spec_threads_format_and_fstype() {
        let mut s = mount_spec(nanosb_protocol::MountType::Bind);
        s.disk_image = Some("raw".to_string());
        s.fstype = Some("ext4".to_string());
        let m = mount_spec_to_mount(&s).unwrap();
        assert_eq!(m.disk_image.as_deref(), Some("raw"));
        assert_eq!(m.fstype.as_deref(), Some("ext4"));
    }

    #[test]
    fn disk_image_without_fstype_is_rejected() {
        let mut s = mount_spec(nanosb_protocol::MountType::Bind);
        s.disk_image = Some("raw".to_string());
        assert!(matches!(
            mount_spec_to_mount(&s),
            Err(MountError::Unsupported(_))
        ));
    }

    #[test]
    fn file_spec_is_threaded() {
        let mut s = mount_spec(nanosb_protocol::MountType::Bind);
        s.file = true;
        let m = mount_spec_to_mount(&s).unwrap();
        assert!(m.file);
    }

    #[test]
    fn options_and_owner_are_threaded() {
        let mut s = mount_spec(nanosb_protocol::MountType::Bind);
        s.options = vec!["noexec".to_string()];
        s.uid = Some(1000);
        s.gid = Some(1000);
        let m = mount_spec_to_mount(&s).unwrap();
        assert_eq!(m.options, vec!["noexec".to_string()]);
        assert_eq!(m.uid, Some(1000));
        assert_eq!(m.gid, Some(1000));
    }

    #[test]
    fn spec_to_config_rejects_missing_mount_host_path() {
        let mut spec = SandboxSpec::new("alpine");
        let mut m = mount_spec(nanosb_protocol::MountType::Bind);
        m.host_path = "/nonexistent/nanosb/path".to_string();
        spec.mounts.push(m);
        let err = spec_to_config(&spec).unwrap_err();
        assert!(
            err.downcast_ref::<MountError>().is_some(),
            "expected MountError, got {err}"
        );
    }

    #[test]
    fn spec_to_config_threads_epic5_fields() {
        use nanosb_protocol::{
            RegistrySpec, RootfsKind, RootfsSpec, SecurityProfile, StatVirtualization,
        };
        let mut spec = SandboxSpec::new("alpine");
        spec.hostname = Some("box".to_string());
        spec.scripts
            .insert("run.sh".to_string(), "echo hi".to_string());
        spec.stat_virtualization = Some(StatVirtualization::Strict);
        spec.security_profile = SecurityProfile::Restricted;
        spec.network.policy = Some(serde_json::json!({"default": "deny"}));
        spec.registry.push(RegistrySpec {
            host: "localhost:5050".to_string(),
            insecure: true,
            skip_tls_verify: false,
            username: None,
            password: None,
        });
        spec.rootfs = Some(RootfsSpec {
            kind: RootfsKind::LocalDir,
            path: Some("/tmp/rootfs".to_string()),
            fstype: None,
            format: None,
            read_only: false,
        });
        let cfg = spec_to_config(&spec).unwrap();
        assert_eq!(cfg.hostname.as_deref(), Some("box"));
        assert_eq!(
            cfg.scripts.get("run.sh").map(String::as_str),
            Some("echo hi")
        );
        assert_eq!(cfg.stat_virtualization.as_deref(), Some("strict"));
        assert_eq!(cfg.security_profile, "restricted");
        assert_eq!(
            cfg.network.policy,
            Some(serde_json::json!({"default": "deny"}))
        );
        assert_eq!(cfg.registry[0].host, "localhost:5050");
        assert!(cfg.registry[0].insecure);
        assert_eq!(
            cfg.rootfs.unwrap().source,
            runtime::config::RootfsSource::LocalDir
        );
    }

    #[test]
    fn every_sandbox_spec_field_reaches_runtime_config() {
        use nanosb_protocol::{
            DockerConfig, DockerNetwork, DockerRuntime, DockerStorageDriver,
            PullPolicy, RegistrySpec, RootfsKind, RootfsPatch, RootfsSpec, SecretEntry,
            SecurityProfile, StatVirtualization,
        };
        let host = tempfile::tempdir().unwrap();
        let mut spec = SandboxSpec::new("alpine");
        spec.name = Some("ac2".to_string());
        spec.cpus = Some(3);
        spec.memory_mb = Some(2048);
        spec.timeout_secs = Some(1234);
        spec.workdir = Some("/w".to_string());
        spec.user = Some("1000".to_string());
        spec.home = Some("/h".to_string());
        spec.env.insert("PLAIN".to_string(), "v".to_string());
        spec.labels.insert("k".to_string(), "v".to_string());
        spec.ephemeral = true;
        spec.pull_policy = PullPolicy::Never;
        spec.command = Some("/bin/true".to_string());
        spec.command_args = vec!["a".to_string()];
        spec.rlimits.push(nanosb_protocol::Rlimit {
            resource: "nofile".to_string(),
            soft: Some(10),
            hard: Some(20),
        });
        spec.scripts
            .insert("s.sh".to_string(), "echo hi".to_string());
        spec.hostname = Some("h".to_string());
        spec.secret_env.insert("API".to_string(), "api".to_string());
        spec.secrets.push(SecretEntry {
            key: "api".to_string(),
            value: Some("secret".to_string()),
        });
        spec.stat_virtualization = Some(StatVirtualization::Strict);
        spec.security_profile = SecurityProfile::Restricted;

        let mut mount = mount_spec(nanosb_protocol::MountType::Bind);
        mount.host_path = host.path().to_string_lossy().to_string();
        mount.options = vec!["noexec".to_string()];
        mount.uid = Some(1);
        mount.gid = Some(2);
        spec.mounts.push(mount);

        spec.network.enabled = false;
        spec.network.mode = Some("none".to_string());
        spec.network.scope = Some("none".to_string());
        spec.network.dns.push("1.1.1.1".to_string());
        spec.network
            .dns_zones
            .insert("z".to_string(), "127.0.0.1".to_string());
        spec.network.policy = Some(serde_json::json!({"default":"deny"}));
        spec.network
            .publish
            .push(nanosb_protocol::PortPublish::udp(1000, 1000));
        spec.registry.push(RegistrySpec {
            host: "h".to_string(),
            insecure: true,
            skip_tls_verify: true,
            username: None,
            password: None,
        });
        spec.rootfs = Some(RootfsSpec {
            kind: RootfsKind::LocalDir,
            path: Some(host.path().to_string_lossy().to_string()),
            fstype: None,
            format: None,
            read_only: false,
        });
        spec.patches.push(RootfsPatch::Mkdir {
            path: "/x".to_string(),
            mode: Some(0o755),
        });
        spec.docker = Some(DockerConfig {
            enabled: true,
            runtime: DockerRuntime::Podman,
            storage_driver: DockerStorageDriver::FuseOverlayfs,
            data_size_bytes: 42 * 1024 * 1024 * 1024,
            network: DockerNetwork::Bridge,
            daemon_json: HashMap::from([(
                "log-driver".to_string(),
                serde_json::from_str::<serde_json::Value>(r#""json-file""#).unwrap(),
            )]),
            preload_images: vec!["busybox:latest".to_string()],
            rootless: true,
            socket: "/run/docker.sock".to_string(),
        });

        let cfg = spec_to_config(&spec).unwrap();
        assert_eq!(cfg.cpus, 3);
        assert_eq!(cfg.memory_mb, 2048);
        assert_eq!(cfg.timeout_secs, 1234);
        assert_eq!(cfg.workdir, "/w");
        assert_eq!(cfg.user.as_deref(), Some("1000"));
        assert_eq!(cfg.home.as_deref(), Some("/h"));
        assert_eq!(cfg.env.get("PLAIN").map(String::as_str), Some("v"));
        assert_eq!(cfg.env.get("API").map(String::as_str), Some("secret"));
        assert_eq!(cfg.labels.get("k").map(String::as_str), Some("v"));
        assert!(cfg.ephemeral);
        assert_eq!(cfg.command.as_deref(), Some("/bin/true"));
        assert_eq!(cfg.command_args, vec!["a".to_string()]);
        assert_eq!(cfg.rlimits, vec!["NOFILE=10:20".to_string()]);
        assert_eq!(cfg.scripts.get("s.sh").map(String::as_str), Some("echo hi"));
        assert_eq!(cfg.hostname.as_deref(), Some("h"));
        assert_eq!(cfg.stat_virtualization.as_deref(), Some("strict"));
        assert_eq!(cfg.security_profile, "restricted");
        assert_eq!(cfg.mounts[0].options, vec!["noexec".to_string()]);
        assert_eq!(cfg.mounts[0].uid, Some(1));
        assert_eq!(cfg.mounts[0].gid, Some(2));
        assert!(!cfg.network.enabled);
        assert_eq!(cfg.network.mode, runtime::config::NetworkMode::None);
        assert_eq!(cfg.network.scope, runtime::config::NetworkScope::None);
        assert_eq!(cfg.network.dns, vec!["1.1.1.1".to_string()]);
        assert_eq!(
            cfg.network.dns_zones.get("z").map(String::as_str),
            Some("127.0.0.1")
        );
        assert_eq!(
            cfg.network.policy,
            Some(serde_json::json!({"default":"deny"}))
        );
        assert_eq!(cfg.network.port_mappings[0].protocol, "udp");
        assert_eq!(cfg.registry[0].host, "h");
        assert!(cfg.registry[0].insecure && cfg.registry[0].skip_tls_verify);
        assert_eq!(
            cfg.rootfs.as_ref().unwrap().source,
            runtime::config::RootfsSource::LocalDir
        );
        assert_eq!(cfg.patches.len(), 1);
        let docker = cfg.docker.as_ref().expect("docker config");
        assert!(docker.enabled);
        assert_eq!(docker.runtime, runtime::config::DockerRuntime::Podman);
        assert_eq!(
            docker.storage_driver,
            runtime::config::DockerStorageDriver::FuseOverlayfs
        );
        assert_eq!(docker.data_size_bytes, 42 * 1024 * 1024 * 1024);
        assert_eq!(docker.network, runtime::config::DockerNetwork::Bridge);
        assert_eq!(
            serde_json::to_string(docker.daemon_json.get("log-driver").unwrap()).unwrap(),
            r#""json-file""#,
        );
        assert_eq!(docker.preload_images, vec!["busybox:latest".to_string()]);
        assert!(docker.rootless);
        assert_eq!(docker.socket, "/run/docker.sock");
    }

    #[test]
    fn docker_absent_maps_to_none() {
        let spec = SandboxSpec::new("alpine");
        let cfg = spec_to_config(&spec).unwrap();
        assert!(cfg.docker.is_none());
    }

    #[test]
    fn secret_env_resolves_from_create_secrets_without_persisting() {
        use nanosb_protocol::SecretEntry;
        let mut spec = SandboxSpec::new("alpine");
        spec.secrets.push(SecretEntry {
            key: "api-key".to_string(),
            value: Some("s3cr3t".to_string()),
        });
        spec.secret_env
            .insert("API_KEY".to_string(), "api-key".to_string());
        let cfg = spec_to_config(&spec).unwrap();
        assert_eq!(cfg.env.get("API_KEY").map(String::as_str), Some("s3cr3t"));
        // The value lives only in config.env, which is stripped into
        // NANOSB_BOOT_ENV before config.json is written.
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("s3cr3t"));
    }

    #[test]
    fn secret_env_missing_secret_is_rejected() {
        let mut spec = SandboxSpec::new("alpine");
        spec.secret_env
            .insert("API_KEY".to_string(), "nope".to_string());
        assert!(spec_to_config(&spec).is_err());
    }

    #[test]
    fn persist_mount_appends_to_stopped_config() {
        let dir = std::env::temp_dir().join(format!("nsb-persist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"name":"t","image":"alpine","mounts":[]}"#,
        )
        .unwrap();
        let mount = Mount::bind("/host/data", "/mnt/data");
        persist_mount(&dir, &mount).unwrap();
        let cfg: SandboxConfig =
            serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).unwrap())
                .unwrap();
        assert_eq!(cfg.mounts.len(), 1);
        assert_eq!(cfg.mounts[0].host_path, PathBuf::from("/host/data"));
        assert_eq!(cfg.mounts[0].container_path, "/mnt/data");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
