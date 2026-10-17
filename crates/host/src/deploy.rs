//! FFI-free deploy helpers shared by the CLI and host-side consumers.

use std::path::{Path, PathBuf};

/// Guest vsock port the exec agent listens on (bridged to a host socket).
pub const EXEC_VSOCK_PORT: u32 = 1024;

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
    let agent_dir = sandbox_dir.join("agent");
    let guest_path = stage_exec_agent(&agent_dir)?;
    let exec_sock = sandbox_dir.join("exec.sock");
    let _ = std::fs::remove_file(&exec_sock);
    {
        use std::io::Write;
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/vsock-diag.log")
        {
            let _ = writeln!(
                f,
                "{ms} prepare_supervisor_spawn: removed {}",
                exec_sock.display()
            );
        }
    }
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
}
