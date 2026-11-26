//! FFI-free deploy helpers shared by the CLI and host-side consumers.

use std::path::{Path, PathBuf};

/// Guest vsock port the exec agent listens on (bridged to a host socket).
pub const EXEC_VSOCK_PORT: u32 = 1024;

/// Default budget for a sandbox to reach `Running` and for its exec channel to
/// come up. A cold start includes the image pull and rootfs extraction, which
/// can exceed one minute for large agent images, so allow five.
pub const SUPERVISOR_START_TIMEOUT_SECS: u64 = 300;

/// Host directory holding the cross-compiled guest binaries (exec agent).
///
/// Resolution order: `NANOSB_GUEST_BIN_DIR`, then `$NANOSB_HOME/guest-bin`,
/// then `~/.nanosandbox/guest-bin`.
pub fn guest_bin_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("NANOSB_GUEST_BIN_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("NANOSB_HOME") {
        return PathBuf::from(dir).join("guest-bin");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".nanosandbox").join("guest-bin")
}

/// Path to the guest exec-agent binary for the host architecture, if staged.
pub fn exec_agent_path() -> Option<PathBuf> {
    let p = guest_bin_dir().join("exec-agent");
    p.exists().then_some(p)
}

/// Stage the exec agent into `dest` for injection into the guest.
///
/// Returns the guest-visible path to run as PID 1.
pub fn stage_exec_agent(dest: &Path) -> anyhow::Result<String> {
    let src = exec_agent_path().ok_or_else(|| {
        anyhow::anyhow!(
            "exec agent not found at {}; build it with:\n  \
             cd guest/crates/exec-agent && \
             RUSTFLAGS=\"-C linker=rust-lld\" cargo build \
             --target aarch64-unknown-linux-musl --release\n  \
             then copy it to {}",
            guest_bin_dir().join("exec-agent").display(),
            guest_bin_dir().display()
        )
    })?;
    std::fs::create_dir_all(dest)?;
    let target = dest.join("exec-agent");
    std::fs::copy(&src, &target)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
    }
    Ok("/agent/exec-agent".to_string())
}

/// Where a sandbox's env/secrets came from, recorded so `restart` can
/// re-resolve them without persisting any secret values.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct Origin {
    /// `"manifest"` (declared in sandbox.yml) or `"cli"` (`nanosb run` flags).
    pub source: String,
    /// Env **key names** only (never values) to re-resolve on restart.
    #[serde(default)]
    pub env_keys: Vec<String>,
    /// `--env-file` path(s) to re-read on restart.
    #[serde(default)]
    pub env_files: Vec<String>,
    /// Path to the declaring `sandbox.yml`, when `source == "manifest"`.
    #[serde(default)]
    pub manifest_path: Option<String>,
}

impl Origin {
    pub fn cli(env_keys: Vec<String>, env_files: Vec<String>) -> Self {
        Self {
            source: "cli".to_string(),
            env_keys,
            env_files,
            manifest_path: None,
        }
    }

    pub fn manifest(manifest_path: Option<String>, env_keys: Vec<String>) -> Self {
        Self {
            source: "manifest".to_string(),
            env_keys,
            env_files: Vec::new(),
            manifest_path,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Limits for inline `agent_config` files (see plan §4.2).
const AGENT_CONFIG_MAX_FILES: usize = 32;
const AGENT_CONFIG_MAX_FILE_BYTES: usize = 256 * 1024;
const AGENT_CONFIG_MAX_TOTAL_BYTES: usize = 1024 * 1024;

/// Validate an `agent_config` key: relative, non-empty, no NUL, no `..`.
fn validate_agent_config_key(key: &str) -> anyhow::Result<()> {
    if key.is_empty() {
        anyhow::bail!("agent_config key must not be empty");
    }
    if key.contains('\0') {
        anyhow::bail!("agent_config key must not contain NUL bytes");
    }
    let p = Path::new(key);
    if p.is_absolute() {
        anyhow::bail!("agent_config key must be relative, got absolute: {key}");
    }
    for comp in p.components() {
        match comp {
            std::path::Component::ParentDir => {
                anyhow::bail!("agent_config key must not contain '..': {key}")
            }
            std::path::Component::CurDir => {
                anyhow::bail!("agent_config key must not contain '.': {key}")
            }
            _ => {}
        }
    }
    if key.split('/').any(|c| c == "." || c.is_empty()) {
        anyhow::bail!("agent_config key must not contain '.' or empty path components: {key}");
    }
    Ok(())
}

/// Reject writing through a symlinked parent component under `root`.
fn reject_symlink_parents(root: &Path, target: &Path) -> anyhow::Result<()> {
    // Reject a symlinked target too: `std::fs::write` follows it, so a link
    // planted at the planned file path could redirect the write outside the
    // sandbox directory.
    if let Ok(meta) = std::fs::symlink_metadata(target) {
        if meta.file_type().is_symlink() {
            anyhow::bail!("agent_config target is a symlink: {}", target.display());
        }
    }
    let rel = target
        .strip_prefix(root)
        .map_err(|_| anyhow::anyhow!("agent_config target escapes the sandbox dir"))?;
    let mut cur = root.to_path_buf();
    for comp in rel.parent().unwrap_or(Path::new("")).components() {
        cur.push(comp);
        match std::fs::symlink_metadata(&cur) {
            Ok(meta) if meta.file_type().is_symlink() => {
                anyhow::bail!(
                    "agent_config parent component is a symlink: {}",
                    cur.display()
                )
            }
            Ok(_) => {}
            Err(_) => break, // component not yet created; nothing to check
        }
    }
    Ok(())
}

/// Split a serialized sandbox config into (config without env, env JSON object).
///
/// Secrets must not be persisted to disk nor placed on the supervisor command
/// line; the env is delivered separately via the supervisor's process environment.
pub fn extract_boot_env(config_json: &str) -> anyhow::Result<(String, String)> {
    let mut cfg: runtime_config::SandboxConfig = serde_json::from_str(config_json)?;
    let env = std::mem::take(&mut cfg.env);
    Ok((serde_json::to_string(&cfg)?, serde_json::to_string(&env)?))
}

/// Everything needed to spawn a supervisor for a `SandboxSpec`, computed once so
/// `nanosb supervise` and the helper (`create_sandbox`) never diverge.
#[derive(Debug, Clone)]
pub struct SupervisorSpawn {
    pub name: String,
    pub sandbox_dir: PathBuf,
    pub config_json: String,
    pub extra_mounts_json: String,
    pub boot_env_json: String,
    pub origin_json: String,
    pub timeout_secs: u32,
}

/// Prepare the on-disk wiring + spawn payload for a sandbox spec: stage the
/// guest exec-agent, set the vsock `exec.sock`, and build the `agent` virtiofs
/// extra mount. Shared by every entry point that spawns a supervisor.
pub fn prepare_supervisor_spawn(
    spec: &nanosb_protocol::SandboxSpec,
) -> anyhow::Result<SupervisorSpawn> {
    let name = spec
        .name
        .clone()
        .ok_or_else(|| anyhow::anyhow!("spec.name is required"))?;
    let mut config = crate::spec::spec_to_config(spec)?;

    let sandbox_dir = crate::supervisor::sandboxes_root().join(&name);

    // Materialize inline agent config files and append file mounts after the
    // spec's existing mounts so they override directory mounts.
    if !spec.agent_config.is_empty() {
        if spec.agent_config.len() > AGENT_CONFIG_MAX_FILES {
            anyhow::bail!("agent_config exceeds the {AGENT_CONFIG_MAX_FILES}-file limit");
        }
        let home = config
            .home
            .clone()
            .unwrap_or_else(|| "/home/developer".to_string());
        let agent_config_dir = sandbox_dir.join("agent-config");
        let mut total_bytes = 0usize;
        for (key, body) in &spec.agent_config {
            validate_agent_config_key(key)?;
            if body.len() > AGENT_CONFIG_MAX_FILE_BYTES {
                anyhow::bail!("agent_config file '{key}' exceeds the 256 KiB limit");
            }
            total_bytes += body.len();
            if total_bytes > AGENT_CONFIG_MAX_TOTAL_BYTES {
                anyhow::bail!("agent_config exceeds the 1 MiB total limit");
            }
            let host_file = agent_config_dir.join(key);
            reject_symlink_parents(&sandbox_dir, &host_file)?;
            if let Some(parent) = host_file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&host_file, body)?;
            let guest_path = format!("{home}/{key}");
            let mut mount = runtime_config::Mount::virtiofs(&host_file, &guest_path);
            mount.file = true;
            mount.readonly = false;
            config.mounts.push(mount);
        }
    }

    let agent_dir = sandbox_dir.join("agent");
    let guest_path = stage_exec_agent(&agent_dir)?;
    let exec_sock = sandbox_dir.join("exec.sock");
    let _ = std::fs::remove_file(&exec_sock);
    let extra_mounts = vec![runtime_config::ExtraMount {
        tag: "agent".to_string(),
        host_path: agent_dir.to_string_lossy().to_string(),
        target: "/agent".to_string(),
        readonly: false,
    }];
    config.vsock_socket = Some(exec_sock.to_string_lossy().to_string());
    config.vsock_port = Some(EXEC_VSOCK_PORT);
    if config.command.is_none() {
        // No workload: the agent itself is PID 1 and serves exec directly.
        config.command = Some(guest_path);
        config.command_args = vec![EXEC_VSOCK_PORT.to_string()];
    } else {
        // Workload present: keep it as PID 1 (exit/stdio/signal contract) and
        // run the agent resident alongside it so exec still works.
        config.exec_agent = Some(guest_path);
    }

    let extra_mounts_json = serde_json::to_string(&extra_mounts)?;
    let config_json = serde_json::to_string(&config)?;
    let (config_json, boot_env_json) = extract_boot_env(&config_json)?;
    let origin_json = Origin::cli(Vec::new(), Vec::new()).to_json();

    Ok(SupervisorSpawn {
        name,
        sandbox_dir,
        config_json,
        extra_mounts_json,
        boot_env_json,
        origin_json,
        timeout_secs: config.timeout_secs,
    })
}

/// Block until the sandbox reports `Running`; fail on `Stopped`/`Error`/timeout.
pub fn wait_for_running(name: &str, timeout_secs: u64) -> anyhow::Result<()> {
    use crate::supervisor::client::SupervisorClient;
    use crate::supervisor::SandboxState;
    let client = SupervisorClient::new(name);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        if let Some(state) = client.read_state() {
            match state.state {
                SandboxState::Running => return Ok(()),
                SandboxState::Stopped | SandboxState::Error => {
                    anyhow::bail!("sandbox failed to start (state: {:?})", state.state)
                }
                SandboxState::Starting => {}
            }
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for sandbox to start");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Block until the guest exec channel (`exec.sock`) appears, or fail on timeout.
pub fn wait_for_exec_channel(name: &str, timeout_secs: u64) -> anyhow::Result<()> {
    let exec_sock = crate::supervisor::sandboxes_root()
        .join(name)
        .join("exec.sock");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    while !exec_sock.exists() {
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for the guest exec channel to come up");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `prepare_supervisor_spawn` reads process-global env vars, so the tests
    /// that exercise it must not run concurrently.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn prepare_spawn_stages_exec_agent_and_wires_vsock() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("nanosb-host-deploy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let guest_bin = tmp.join("guest-bin");
        std::fs::create_dir_all(&guest_bin).unwrap();
        std::fs::write(guest_bin.join("exec-agent"), b"#!/bin/sh\n").unwrap();

        std::env::set_var("NANOSB_GUEST_BIN_DIR", &guest_bin);
        std::env::set_var("NANOSB_HOME", tmp.join("home"));

        let mut spec = nanosb_protocol::SandboxSpec::new("alpine");
        spec.name = Some("host-deploy-test".to_string());
        let sp = prepare_supervisor_spawn(&spec).unwrap();

        let cfg: runtime_config::SandboxConfig = serde_json::from_str(&sp.config_json).unwrap();
        assert_eq!(cfg.vsock_port, Some(EXEC_VSOCK_PORT));
        assert!(cfg
            .vsock_socket
            .as_deref()
            .is_some_and(|s| s.ends_with("exec.sock")));
        assert_eq!(cfg.command.as_deref(), Some("/agent/exec-agent"));

        let mounts: Vec<runtime_config::ExtraMount> =
            serde_json::from_str(&sp.extra_mounts_json).unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].tag, "agent");
        assert_eq!(mounts[0].target, "/agent");
        assert!(sp.sandbox_dir.join("agent/exec-agent").exists());

        std::env::remove_var("NANOSB_GUEST_BIN_DIR");
        std::env::remove_var("NANOSB_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn prepare_spawn_keeps_workload_and_runs_agent_resident() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!(
            "nanosb-host-deploy-resident-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let guest_bin = tmp.join("guest-bin");
        std::fs::create_dir_all(&guest_bin).unwrap();
        std::fs::write(guest_bin.join("exec-agent"), b"#!/bin/sh\n").unwrap();

        std::env::set_var("NANOSB_GUEST_BIN_DIR", &guest_bin);
        std::env::set_var("NANOSB_HOME", tmp.join("home"));

        let mut spec = nanosb_protocol::SandboxSpec::new("alpine");
        spec.name = Some("host-deploy-resident-test".to_string());
        spec.command = Some("/bin/sleep".to_string());
        spec.command_args = vec!["60".to_string()];
        let sp = prepare_supervisor_spawn(&spec).unwrap();

        let cfg: runtime_config::SandboxConfig = serde_json::from_str(&sp.config_json).unwrap();
        assert_eq!(cfg.command.as_deref(), Some("/bin/sleep"));
        assert_eq!(cfg.command_args, vec!["60".to_string()]);
        assert_eq!(cfg.exec_agent.as_deref(), Some("/agent/exec-agent"));

        std::env::remove_var("NANOSB_GUEST_BIN_DIR");
        std::env::remove_var("NANOSB_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn setup_env(tmp: &Path) {
        let guest_bin = tmp.join("guest-bin");
        std::fs::create_dir_all(&guest_bin).unwrap();
        std::fs::write(guest_bin.join("exec-agent"), b"#!/bin/sh\n").unwrap();
        std::env::set_var("NANOSB_GUEST_BIN_DIR", &guest_bin);
        std::env::set_var("NANOSB_HOME", tmp.join("home"));
    }

    fn teardown_env(tmp: &Path) {
        std::env::remove_var("NANOSB_GUEST_BIN_DIR");
        std::env::remove_var("NANOSB_HOME");
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn prepare_spawn_materializes_agent_config_and_appends_file_mount() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp =
            std::env::temp_dir().join(format!("nanosb-host-agent-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        setup_env(&tmp);

        let mut spec = nanosb_protocol::SandboxSpec::new("alpine");
        spec.name = Some("agent-config-test".to_string());
        spec.home = Some("/home/developer".to_string());
        spec.agent_config
            .insert(".omo/omo.jsonc".into(), "{\"categories\":{}}".into());
        let sp = prepare_supervisor_spawn(&spec).unwrap();

        let cfg: runtime_config::SandboxConfig = serde_json::from_str(&sp.config_json).unwrap();
        // The file mount is appended after the spec's mounts (none here).
        assert_eq!(cfg.mounts.len(), 1);
        let m = &cfg.mounts[0];
        assert!(m.file);
        assert!(!m.readonly);
        assert_eq!(m.container_path, "/home/developer/.omo/omo.jsonc");
        assert!(m.host_path.ends_with("agent-config/.omo/omo.jsonc"));
        // File written byte-identical.
        let written = std::fs::read_to_string(&m.host_path).unwrap();
        assert_eq!(written, "{\"categories\":{}}");

        teardown_env(&tmp);
    }

    #[test]
    fn prepare_spawn_rejects_agent_config_traversal() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!(
            "nanosb-host-agent-config-traversal-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        setup_env(&tmp);

        for bad in [
            "../escape",
            "/abs/path",
            "a/../b",
            "a/./b",
            "a//b",
            "a/",
            "./a",
            "",
        ] {
            let mut spec = nanosb_protocol::SandboxSpec::new("alpine");
            spec.name = Some("agent-config-traversal-test".to_string());
            spec.agent_config.insert(bad.to_string(), "x".into());
            assert!(
                prepare_supervisor_spawn(&spec).is_err(),
                "expected rejection for key {bad:?}"
            );
        }

        teardown_env(&tmp);
    }

    #[test]
    fn reject_symlink_parents_rejects_symlinked_target() {
        let dir =
            std::env::temp_dir().join(format!("nanosb-host-symlink-target-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sandbox_dir = dir.join("sandbox");
        std::fs::create_dir_all(sandbox_dir.join("agent-config").join(".omo")).unwrap();
        let outside = dir.join("outside.jsonc");
        std::fs::write(&outside, "{}").unwrap();
        let target = sandbox_dir
            .join("agent-config")
            .join(".omo")
            .join("omo.jsonc");
        std::os::unix::fs::symlink(&outside, &target).unwrap();

        let err = reject_symlink_parents(&sandbox_dir, &target).unwrap_err();
        assert!(err.to_string().contains("symlink"), "got: {err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_spawn_enforces_agent_config_limits() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!(
            "nanosb-host-agent-config-limits-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        setup_env(&tmp);

        // Per-file limit: 256 KiB + 1.
        let mut spec = nanosb_protocol::SandboxSpec::new("alpine");
        spec.name = Some("agent-config-limit-test".to_string());
        spec.agent_config
            .insert("big".into(), "x".repeat(256 * 1024 + 1));
        assert!(prepare_supervisor_spawn(&spec).is_err());

        // File-count limit: 33 files.
        let mut spec = nanosb_protocol::SandboxSpec::new("alpine");
        spec.name = Some("agent-config-limit-test".to_string());
        for i in 0..33 {
            spec.agent_config.insert(format!("f{i}"), "x".into());
        }
        assert!(prepare_supervisor_spawn(&spec).is_err());

        // Total limit: 1 MiB + 1 across two files.
        let mut spec = nanosb_protocol::SandboxSpec::new("alpine");
        spec.name = Some("agent-config-limit-test".to_string());
        spec.agent_config.insert("a".into(), "x".repeat(512 * 1024));
        spec.agent_config
            .insert("b".into(), "x".repeat(512 * 1024 + 1));
        assert!(prepare_supervisor_spawn(&spec).is_err());

        teardown_env(&tmp);
    }
}
