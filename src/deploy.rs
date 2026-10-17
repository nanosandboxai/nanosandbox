//! Deploy planning and supervisor spawning, shared by the CLI and TUI.
//!
//! The FFI-free pieces (exec-agent staging, origin tracking, boot-env splitting)
//! now live in `nanosb-host` and are re-exported here for compatibility.

use crate::supervisor::client::SupervisorClient;

pub use nanosb_host::deploy::{
    exec_agent_path, extract_boot_env, guest_bin_dir, prepare_supervisor_spawn, stage_exec_agent,
    wait_for_exec_channel, wait_for_running, Origin, SupervisorSpawn, EXEC_VSOCK_PORT,
};

/// Spawn a detached supervisor process for `sandbox_name`.
pub fn spawn_supervisor(
    sandbox_name: &str,
    config_json: &str,
    extra_mounts_json: &str,
    boot_env_json: &str,
    origin_json: &str,
    timeout_secs: u32,
) -> anyhow::Result<()> {
    spawn_supervisor_with_pid(
        sandbox_name,
        config_json,
        extra_mounts_json,
        boot_env_json,
        origin_json,
        timeout_secs,
    )
    .map(|_| ())
}

/// Spawn a detached supervisor process and return its pid. The child is
/// re-parented to init; use [`spawn_supervisor_foreground`] when the caller
/// must own and reap it.
pub fn spawn_supervisor_with_pid(
    sandbox_name: &str,
    config_json: &str,
    extra_mounts_json: &str,
    boot_env_json: &str,
    origin_json: &str,
    timeout_secs: u32,
) -> anyhow::Result<u32> {
    let sp = SupervisorSpawn {
        name: sandbox_name.to_string(),
        sandbox_dir: SupervisorClient::new(sandbox_name)
            .sandbox_dir()
            .to_path_buf(),
        config_json: config_json.to_string(),
        extra_mounts_json: extra_mounts_json.to_string(),
        boot_env_json: boot_env_json.to_string(),
        origin_json: origin_json.to_string(),
        timeout_secs,
    };
    let child = spawn_supervisor_child(&sp, false)?;
    let pid = child.id();
    drop(child);
    Ok(pid)
}

/// Spawn the supervisor as a live child of the caller (no `setsid`, no `drop`),
/// so an external parent (the shim) owns and reaps it. Used by
/// `nanosb supervise --foreground`.
pub fn spawn_supervisor_foreground(sp: &SupervisorSpawn) -> anyhow::Result<std::process::Child> {
    spawn_supervisor_child(sp, true)
}

/// Map a `wait()` status to the task exit code: the exit code, or `128 + signal`
/// when the process was killed by a signal (the #164 contract).
pub fn exit_code_from_status(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    -1
}

fn spawn_supervisor_child(
    sp: &SupervisorSpawn,
    foreground: bool,
) -> anyhow::Result<std::process::Child> {
    use std::os::unix::fs::PermissionsExt;
    // Tests run under the cargo test harness binary, which does not know the
    // `__supervise` subcommand. Honor NANOSB_BINARY_PATH (as the runtime does)
    // so tests can spawn the real nanosb binary.
    let exe = match std::env::var("NANOSB_BINARY_PATH") {
        Ok(path) if !path.is_empty() => std::path::PathBuf::from(path),
        _ => std::env::current_exe()?,
    };
    std::fs::create_dir_all(sp.sandbox_dir.join("logs"))?;
    let config_path = sp.sandbox_dir.join("config.json");
    std::fs::write(&config_path, &sp.config_json)?;
    let deploy = serde_json::json!({
        "config_json": sp.config_json,
        "extra_mounts_json": sp.extra_mounts_json,
    });
    let deploy_path = sp.sandbox_dir.join("deploy.json");
    std::fs::write(&deploy_path, deploy.to_string())?;
    let origin_path = sp.sandbox_dir.join("origin.json");
    std::fs::write(&origin_path, &sp.origin_json)?;
    let supervisor_log_path = sp.sandbox_dir.join("logs").join("supervisor.log");
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
        .arg(&sp.name)
        .arg(&sp.config_json)
        .arg("--extra-mounts-json")
        .arg(&sp.extra_mounts_json)
        .arg("--timeout-secs")
        .arg(sp.timeout_secs.to_string())
        .env("NANOSB_BOOT_ENV", &sp.boot_env_json)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(supervisor_log))
        .stderr(std::process::Stdio::from(supervisor_log_err));
    if foreground {
        cmd.arg("--foreground");
    }
    cmd.spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn supervisor: {}", e))
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
            interactive: config.interactive,
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
    fn foreground_spawn_is_reapable_and_maps_exit_code() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = std::env::temp_dir().join(format!("nanosb-fg-spawn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let fake = tmp.join("fake-supervisor.sh");
        std::fs::write(&fake, "#!/bin/sh\nexit 7\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        std::env::set_var("NANOSB_BINARY_PATH", &fake);
        let sp = SupervisorSpawn {
            name: "fg-spawn-test".to_string(),
            sandbox_dir: tmp.join("sandbox"),
            config_json: "{}".to_string(),
            extra_mounts_json: "[]".to_string(),
            boot_env_json: "{}".to_string(),
            origin_json: "{}".to_string(),
            timeout_secs: 1,
        };
        let mut child = spawn_supervisor_foreground(&sp).unwrap();
        let status = child.wait().unwrap();
        assert_eq!(exit_code_from_status(status), 7);
        assert!(sp.sandbox_dir.join("logs/supervisor.log").exists());

        std::env::remove_var("NANOSB_BINARY_PATH");
        let _ = std::fs::remove_dir_all(&tmp);
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
