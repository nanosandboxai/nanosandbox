//! Deploy planning and supervisor spawning, shared by the CLI and TUI.
//!
//! These helpers were extracted from the `main.rs` binary crate so the TUI
//! (which lives in the `nanosb_cli` library crate) can spawn supervisor-managed
//! sandboxes through the exact same code path as `nanosb run`/`nanosb apply`.

use crate::supervisor::client::SupervisorClient;

/// Guest vsock port the exec agent listens on (bridged to a host socket).
pub const EXEC_VSOCK_PORT: u32 = 1024;

/// Host directory holding the cross-compiled guest binaries (exec agent).
///
/// Overridable with `NANOSB_GUEST_BIN_DIR`; defaults to
/// `~/.nanosandbox/guest-bin`.
pub fn guest_bin_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("NANOSB_GUEST_BIN_DIR") {
        return std::path::PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    std::path::PathBuf::from(home)
        .join(".nanosandbox")
        .join("guest-bin")
}

/// Path to the guest exec-agent binary for the host architecture.
///
/// The agent is cross-compiled for the guest (aarch64-linux-musl) and staged
/// here. Returns `None` if it has not been built/staged yet.
pub fn exec_agent_path() -> Option<std::path::PathBuf> {
    let name = "exec-agent";
    let p = guest_bin_dir().join(name);
    p.exists().then_some(p)
}

/// Stage the exec agent into `dest` for injection into the guest.
///
/// `dest` is a host directory added to the deploy plan as a read-only virtiofs
/// mount at `/agent`. Returns the guest-visible path to run as PID 1.
pub fn stage_exec_agent(dest: &std::path::Path) -> anyhow::Result<String> {
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
/// Secrets must not be persisted to `config.json`/`deploy.json` nor placed on
/// the supervisor command line; the caller passes the env separately so it can
/// be delivered via the supervisor's process environment instead.
pub fn extract_boot_env(config_json: &str) -> anyhow::Result<(String, String)> {
    let mut cfg: runtime::config::SandboxConfig = serde_json::from_str(config_json)?;
    let env = std::mem::take(&mut cfg.env);
    Ok((serde_json::to_string(&cfg)?, serde_json::to_string(&env)?))
}

/// Spawn a detached supervisor process for `sandbox_name`.
///
/// Writes `config.json`, `deploy.json`, `origin.json`, and `logs/supervisor.log`
/// (all 0600) into the sandbox dir, then execs the current binary as
/// `__supervise`. `boot_env_json` is passed via the child's environment (never
/// argv or disk) and merged into the guest boot env by the supervisor.
pub fn spawn_supervisor(
    sandbox_name: &str,
    config_json: &str,
    extra_mounts_json: &str,
    boot_env_json: &str,
    origin_json: &str,
    timeout_secs: u32,
) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let exe = std::env::current_exe()?;
    let sandbox_dir = SupervisorClient::new(sandbox_name)
        .sandbox_dir()
        .to_path_buf();
    std::fs::create_dir_all(sandbox_dir.join("logs"))?;
    let config_path = sandbox_dir.join("config.json");
    std::fs::write(&config_path, config_json)?;
    let deploy = serde_json::json!({
        "config_json": config_json,
        "extra_mounts_json": extra_mounts_json,
    });
    let deploy_path = sandbox_dir.join("deploy.json");
    std::fs::write(&deploy_path, deploy.to_string())?;
    let origin_path = sandbox_dir.join("origin.json");
    std::fs::write(&origin_path, origin_json)?;
    let supervisor_log_path = sandbox_dir.join("logs").join("supervisor.log");
    let supervisor_log = std::fs::File::create(&supervisor_log_path)?;
    for path in [
        &config_path,
        &deploy_path,
        &origin_path,
        &supervisor_log_path,
    ] {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    let supervisor_log_err = supervisor_log.try_clone()?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("__supervise")
        .arg(sandbox_name)
        .arg(config_json)
        .arg("--extra-mounts-json")
        .arg(extra_mounts_json)
        .arg("--timeout-secs")
        .arg(timeout_secs.to_string())
        .env("NANOSB_BOOT_ENV", boot_env_json)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(supervisor_log))
        .stderr(std::process::Stdio::from(supervisor_log_err));
    let child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn supervisor: {}", e))?;
    drop(child);
    Ok(())
}

/// Poll until the supervisor reports `Running`, `Error`, or the timeout elapses.
pub async fn wait_supervisor_running(
    client: &SupervisorClient,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        if let Some(state) = client.read_state() {
            match state.state {
                crate::supervisor::SandboxState::Running => return Ok(()),
                crate::supervisor::SandboxState::Error => {
                    anyhow::bail!("sandbox failed to start (see supervisor.log)")
                }
                _ => {}
            }
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!("timed out waiting for sandbox to start");
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// Materialize a deploy plan on disk: create host mount roots and write config files.
pub fn materialize_plan(
    sandbox_dir: &std::path::Path,
    plan: &sandbox::deploy::DeployPlan,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(sandbox_dir)?;
    for mount in &plan.mounts {
        let host = &mount.host_path;
        if host.starts_with(sandbox_dir) {
            std::fs::create_dir_all(host)?;
        } else if !host.exists() {
            anyhow::bail!("mount host path does not exist: {}", host.display());
        }
    }
    for file in &plan.config_files {
        let path = match sandbox::deploy::merged_config_path(&file.relative_path) {
            Some(merged) => sandbox_dir.join("state").join(merged),
            None => sandbox_dir.join("config").join(&file.relative_path),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &file.content)?;
    }
    Ok(())
}

/// Convert a deploy plan's mounts into runtime `ExtraMount` entries.
pub fn extra_mounts_from_plan(
    plan: &sandbox::deploy::DeployPlan,
) -> Vec<runtime::config::ExtraMount> {
    plan.mounts
        .iter()
        .enumerate()
        .map(|(index, mount)| runtime::config::ExtraMount {
            tag: format!("nanosb-{}", index),
            host_path: mount.host_path.to_string_lossy().to_string(),
            target: mount.guest_path.clone(),
            readonly: mount.readonly,
        })
        .collect()
}

/// Build the deploy plan and next-mode runtime config for an agent sandbox config.
pub fn deploy_plan_for(
    config: &sandbox::AgentSandboxConfig,
    sandbox_dir: &std::path::Path,
) -> (sandbox::deploy::DeployPlan, runtime::config::SandboxConfig) {
    let resolved = config
        .resolved_agent
        .clone()
        .unwrap_or_else(|| sandbox::ResolvedAgentConfig {
            agent_name: config
                .agent
                .clone()
                .unwrap_or_else(|| "default".to_string()),
            prompt: config.prompt.clone().unwrap_or_default(),
            skills: Vec::new(),
            mcp_servers: config.mcp_servers.clone(),
            auto_mode: config.auto_mode,
            permissions: config.permissions,
            agent_type: config.agent_type,
            claude_settings: config.claude_settings.clone(),
        });
    let plan = sandbox::deploy::DeployPlanner::plan(config, &resolved, sandbox_dir, None);
    let mut rc = config.sandbox.clone();
    rc.runtime_mode = runtime::config::RuntimeMode::Next;
    let has_agent = config.agent_type.is_some()
        || config.agent.is_some()
        || config.prompt.is_some()
        || config.auto_mode;
    if has_agent && rc.user.is_none() && !rc.run_as_root {
        rc.user = Some("developer".to_string());
        if rc.home.is_none() {
            rc.home = Some("/home/developer".to_string());
        }
    }
    if has_agent {
        rc.command = Some(plan.agent_command.binary.clone());
        rc.command_args = plan.agent_command.args.clone();
        let agent_type = config.agent_type.unwrap_or(sandbox::AgentType::Claude);
        rc.env = sandbox::deploy::AgentCommandBuilder::build_env(
            &agent_type,
            config.auto_mode,
            config.permissions,
            &plan.env,
        );
    }
    (plan, rc)
}

/// SHA-256 hex digest of a string (used for deploy config hashing).
pub fn sha256_hex(data: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_env(env: &[(&str, &str)]) -> String {
        let mut c = runtime::config::SandboxConfig::builder()
            .name("t")
            .image("alpine")
            .build();
        for (k, v) in env {
            c.env.insert((*k).to_string(), (*v).to_string());
        }
        serde_json::to_string(&c).unwrap()
    }

    #[test]
    fn extract_boot_env_removes_env_from_config() {
        let json = cfg_with_env(&[("SECRET", "s3cr3t"), ("PLAIN", "x")]);
        let (cfg_json, env_json) = extract_boot_env(&json).unwrap();
        let cfg: runtime::config::SandboxConfig = serde_json::from_str(&cfg_json).unwrap();
        assert!(cfg.env.is_empty(), "config must carry no env");
        let env: std::collections::HashMap<String, String> =
            serde_json::from_str(&env_json).unwrap();
        assert_eq!(env.get("SECRET").map(String::as_str), Some("s3cr3t"));
        assert_eq!(env.get("PLAIN").map(String::as_str), Some("x"));
    }

    #[test]
    fn extract_boot_env_without_env_yields_empty_map() {
        let json = cfg_with_env(&[]);
        let (cfg_json, env_json) = extract_boot_env(&json).unwrap();
        let cfg: runtime::config::SandboxConfig = serde_json::from_str(&cfg_json).unwrap();
        assert!(cfg.env.is_empty());
        let env: std::collections::HashMap<String, String> =
            serde_json::from_str(&env_json).unwrap();
        assert!(env.is_empty());
    }

    #[test]
    fn extract_boot_env_roundtrip_preserves_non_env_fields() {
        let json = cfg_with_env(&[("K", "v")]);
        let (cfg_json, _) = extract_boot_env(&json).unwrap();
        let cfg: runtime::config::SandboxConfig = serde_json::from_str(&cfg_json).unwrap();
        assert_eq!(cfg.name, "t");
        assert_eq!(cfg.image, "alpine");
    }

    #[test]
    fn origin_cli_json_shape() {
        let o = Origin::cli(vec!["A".into(), "B".into()], vec!["/tmp/e".into()]);
        let v: serde_json::Value = serde_json::from_str(&o.to_json()).unwrap();
        assert_eq!(v["source"], "cli");
        assert_eq!(v["env_keys"][0], "A");
        assert_eq!(v["env_files"][0], "/tmp/e");
        assert!(v["manifest_path"].is_null());
    }

    #[test]
    fn origin_manifest_json_shape() {
        let o = Origin::manifest(Some("/p/sandbox.yml".into()), vec!["A".into()]);
        let v: serde_json::Value = serde_json::from_str(&o.to_json()).unwrap();
        assert_eq!(v["source"], "manifest");
        assert_eq!(v["manifest_path"], "/p/sandbox.yml");
    }

    #[test]
    fn origin_default_has_empty_source() {
        let o = Origin::default();
        assert!(o.source.is_empty());
    }

    #[test]
    fn deploy_plan_defaults_agent_to_developer() {
        let mut config = sandbox::AgentSandboxConfig::default();
        config.sandbox.name = "d".to_string();
        config.sandbox.image = "claude".to_string();
        config.agent_type = Some(sandbox::AgentType::Claude);
        let dir = std::env::temp_dir().join("nanosb-deploy-test-defaults");
        let (_plan, rc) = deploy_plan_for(&config, &dir);
        assert_eq!(rc.user.as_deref(), Some("developer"));
        assert_eq!(rc.home.as_deref(), Some("/home/developer"));
    }

    #[test]
    fn deploy_plan_respects_explicit_user() {
        let mut config = sandbox::AgentSandboxConfig::default();
        config.sandbox.name = "d".to_string();
        config.sandbox.image = "claude".to_string();
        config.sandbox.user = Some("2000".to_string());
        config.agent_type = Some(sandbox::AgentType::Claude);
        let dir = std::env::temp_dir().join("nanosb-deploy-test-explicit");
        let (_plan, rc) = deploy_plan_for(&config, &dir);
        assert_eq!(rc.user.as_deref(), Some("2000"));
    }

    #[test]
    fn deploy_plan_run_as_root_not_overridden() {
        let mut config = sandbox::AgentSandboxConfig::default();
        config.sandbox.name = "d".to_string();
        config.sandbox.image = "claude".to_string();
        config.sandbox.run_as_root = true;
        config.agent_type = Some(sandbox::AgentType::Claude);
        let dir = std::env::temp_dir().join("nanosb-deploy-test-root");
        let (_plan, rc) = deploy_plan_for(&config, &dir);
        assert_ne!(rc.user.as_deref(), Some("developer"));
    }
}
