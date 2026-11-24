//! Fluent builder for a sandbox creation spec.

use std::collections::HashMap;

use nanosb_protocol::{
    MountSpec, MountType, NetworkSpec, PortPublish, PullPolicy, RegistrySpec, Rlimit, RootfsPatch,
    RootfsSpec, SandboxSpec, SecretEntry, SecurityProfile, StatVirtualization, VolumeSpec,
};

/// Builds a [`SandboxSpec`] with a fluent API, mirroring the shape of the other
/// language SDKs.
#[derive(Debug, Clone)]
pub struct SandboxBuilder {
    spec: SandboxSpec,
}

impl Default for SandboxBuilder {
    fn default() -> Self {
        Self::new("")
    }
}

impl SandboxBuilder {
    pub fn new(image: impl Into<String>) -> Self {
        Self {
            spec: SandboxSpec::new(image),
        }
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.spec.name = Some(name.into());
        self
    }

    pub fn cpus(mut self, cpus: u32) -> Self {
        self.spec.cpus = Some(cpus);
        self
    }

    pub fn memory_mb(mut self, mb: u32) -> Self {
        self.spec.memory_mb = Some(mb);
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.spec.env.insert(key.into(), value.into());
        self
    }

    pub fn workdir(mut self, dir: impl Into<String>) -> Self {
        self.spec.workdir = Some(dir.into());
        self
    }

    pub fn timeout_secs(mut self, secs: u32) -> Self {
        self.spec.timeout_secs = Some(secs);
        self
    }

    pub fn user(mut self, user: impl Into<String>) -> Self {
        self.spec.user = Some(user.into());
        self
    }

    pub fn home(mut self, home: impl Into<String>) -> Self {
        self.spec.home = Some(home.into());
        self
    }

    pub fn command(mut self, cmd: impl Into<String>, args: &[&str]) -> Self {
        self.spec.command = Some(cmd.into());
        self.spec.command_args = args.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn mount(mut self, host: impl Into<String>, guest: impl Into<String>) -> Self {
        self.spec.mounts.push(MountSpec::virtiofs(host, guest));
        self
    }

    pub fn mount_readonly(mut self, host: impl Into<String>, guest: impl Into<String>) -> Self {
        let mut m = MountSpec::virtiofs(host, guest);
        m.readonly = true;
        self.spec.mounts.push(m);
        self
    }

    pub fn mount_spec(mut self, mount: MountSpec) -> Self {
        self.spec.mounts.push(mount);
        self
    }

    /// Mount a single host file at `guest`.
    pub fn mount_file(mut self, host: impl Into<String>, guest: impl Into<String>) -> Self {
        let mut m = MountSpec::virtiofs(host, guest);
        m.file = true;
        self.spec.mounts.push(m);
        self
    }

    /// Mount a tmpfs at `guest` (host path is ignored).
    pub fn mount_tmpfs(mut self, guest: impl Into<String>) -> Self {
        let mut m = MountSpec::bind("", guest);
        m.tmpfs = true;
        m.mount_type = MountType::Tmpfs;
        self.spec.mounts.push(m);
        self
    }

    /// Apply mount options (e.g. `noexec`, `nosuid`) to the most recently
    /// added mount.
    pub fn mount_options(mut self, options: &[&str]) -> Self {
        if let Some(m) = self.spec.mounts.last_mut() {
            m.options = options.iter().map(|s| s.to_string()).collect();
        }
        self
    }

    /// Set the uid/gid owner of the most recently added mount.
    pub fn mount_owner(mut self, uid: u32, gid: u32) -> Self {
        if let Some(m) = self.spec.mounts.last_mut() {
            m.uid = Some(uid);
            m.gid = Some(gid);
        }
        self
    }

    pub fn publish_tcp(mut self, host_port: u16, guest_port: u16) -> Self {
        self.spec
            .network
            .publish
            .push(PortPublish::tcp(host_port, guest_port));
        self
    }

    pub fn publish_udp(mut self, host_port: u16, guest_port: u16) -> Self {
        self.spec
            .network
            .publish
            .push(PortPublish::udp(host_port, guest_port));
        self
    }

    pub fn publish(mut self, port: PortPublish) -> Self {
        self.spec.network.publish.push(port);
        self
    }

    pub fn network(mut self, network: NetworkSpec) -> Self {
        self.spec.network = network;
        self
    }

    pub fn rlimit(mut self, resource: impl Into<String>, soft: u64, hard: u64) -> Self {
        self.spec.rlimits.push(Rlimit {
            resource: resource.into(),
            soft: Some(soft),
            hard: Some(hard),
        });
        self
    }

    pub fn label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.spec.labels.insert(key.into(), value.into());
        self
    }

    pub fn labels(mut self, labels: HashMap<String, String>) -> Self {
        self.spec.labels.extend(labels);
        self
    }

    pub fn ephemeral(mut self, ephemeral: bool) -> Self {
        self.spec.ephemeral = ephemeral;
        self
    }

    pub fn pull_policy(mut self, policy: PullPolicy) -> Self {
        self.spec.pull_policy = policy;
        self
    }

    pub fn script(mut self, name: impl Into<String>, body: impl Into<String>) -> Self {
        self.spec.scripts.insert(name.into(), body.into());
        self
    }

    /// Supply an inline agent-native config file. `key` is guest-relative to
    /// the guest home (e.g. `.omo/omo.jsonc`); `body` is the raw file content.
    pub fn agent_config(mut self, key: impl Into<String>, body: impl Into<String>) -> Self {
        self.spec.agent_config.insert(key.into(), body.into());
        self
    }

    pub fn replace(mut self, replace: bool) -> Self {
        self.spec.replace = replace;
        self
    }

    /// Start detached (helper owns the lifecycle).
    pub fn detached(mut self, detached: bool) -> Self {
        self.spec.detached = detached;
        self
    }

    /// Override the guest hostname.
    pub fn hostname(mut self, hostname: impl Into<String>) -> Self {
        self.spec.hostname = Some(hostname.into());
        self
    }

    /// Source an env key from the secret store: `key` is the env var name,
    /// `secret_key` the secret store entry it resolves to.
    pub fn secret_env(mut self, key: impl Into<String>, secret_key: impl Into<String>) -> Self {
        self.spec.secret_env.insert(key.into(), secret_key.into());
        self
    }

    /// Supply a named secret value at create time (never persisted). Reference
    /// it from an env key with [`SandboxBuilder::secret_env`].
    pub fn secret(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.spec.secrets.push(SecretEntry {
            key: key.into(),
            value: Some(value.into()),
        });
        self
    }

    /// Set a non-OCI rootfs source (local directory or disk image).
    pub fn rootfs(mut self, rootfs: RootfsSpec) -> Self {
        self.spec.rootfs = Some(rootfs);
        self
    }

    /// Add a pre-boot rootfs patch.
    pub fn patch(mut self, patch: RootfsPatch) -> Self {
        self.spec.patches.push(patch);
        self
    }

    /// Add a per-sandbox registry configuration entry.
    pub fn registry(mut self, registry: RegistrySpec) -> Self {
        self.spec.registry.push(registry);
        self
    }

    /// Set the stat virtualization mode.
    pub fn stat_virtualization(mut self, mode: StatVirtualization) -> Self {
        self.spec.stat_virtualization = Some(mode);
        self
    }

    /// Set the guest security profile.
    pub fn security_profile(mut self, profile: SecurityProfile) -> Self {
        self.spec.security_profile = profile;
        self
    }

    pub fn spec(&self) -> &SandboxSpec {
        &self.spec
    }

    pub fn build(self) -> SandboxSpec {
        self.spec
    }
}

/// Builds a [`VolumeSpec`] for `volume_create`.
#[derive(Debug, Clone)]
pub struct VolumeBuilder {
    spec: VolumeSpec,
}

impl VolumeBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            spec: VolumeSpec {
                name: name.into(),
                dir: None,
                disk_image: None,
                fstype: None,
                quota_bytes: None,
            },
        }
    }

    pub fn dir(mut self, dir: impl Into<String>) -> Self {
        self.spec.dir = Some(dir.into());
        self
    }

    pub fn disk_image(mut self, image: impl Into<String>) -> Self {
        self.spec.disk_image = Some(image.into());
        self
    }

    pub fn fstype(mut self, fstype: impl Into<String>) -> Self {
        self.spec.fstype = Some(fstype.into());
        self
    }

    /// Cap the volume at `bytes`.
    pub fn volume_quota(mut self, bytes: u64) -> Self {
        self.spec.quota_bytes = Some(bytes);
        self
    }

    pub fn build(self) -> VolumeSpec {
        self.spec
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_full_spec() {
        let spec = SandboxBuilder::new("alpine:latest")
            .name("demo")
            .cpus(2)
            .memory_mb(512)
            .env("K", "V")
            .publish_tcp(8080, 80)
            .publish_udp(53, 53)
            .label("team", "sdk")
            .ephemeral(true)
            .build();
        assert_eq!(spec.name.as_deref(), Some("demo"));
        assert_eq!(spec.cpus, Some(2));
        assert_eq!(spec.network.publish.len(), 2);
        assert_eq!(spec.network.publish[1].protocol, "udp");
        assert!(spec.ephemeral);
        assert_eq!(spec.env.get("K").map(String::as_str), Some("V"));
    }

    #[test]
    fn builds_mount_and_secret_methods() {
        let spec = SandboxBuilder::new("alpine")
            .detached(false)
            .hostname("box")
            .mount_file("/host/f", "/guest/f")
            .mount_tmpfs("/tmp")
            .mount_options(&["noexec", "nosuid"])
            .mount_owner(1000, 1000)
            .secret_env("API_KEY", "api-key")
            .build();
        assert!(!spec.detached);
        assert_eq!(spec.hostname.as_deref(), Some("box"));
        assert!(spec.mounts[0].file);
        assert!(spec.mounts[1].tmpfs);
        assert_eq!(spec.mounts[1].mount_type, MountType::Tmpfs);
        assert_eq!(spec.mounts[1].options, vec!["noexec", "nosuid"]);
        assert_eq!(spec.mounts[1].uid, Some(1000));
        assert_eq!(spec.mounts[1].gid, Some(1000));
        assert_eq!(
            spec.secret_env.get("API_KEY").map(String::as_str),
            Some("api-key")
        );
    }

    #[test]
    fn builds_agent_config() {
        let spec = SandboxBuilder::new("alpine")
            .agent_config(".omo/omo.jsonc", "{\"categories\":{}}")
            .build();
        assert_eq!(
            spec.agent_config.get(".omo/omo.jsonc").map(String::as_str),
            Some("{\"categories\":{}}")
        );
    }

    #[test]
    fn builds_a_volume_spec() {
        let vol = VolumeBuilder::new("data")
            .dir("/tmp/data")
            .volume_quota(1024)
            .build();
        assert_eq!(vol.name, "data");
        assert_eq!(vol.dir.as_deref(), Some("/tmp/data"));
        assert_eq!(vol.quota_bytes, Some(1024));
    }
}
