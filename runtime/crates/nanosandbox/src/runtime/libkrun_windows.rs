//! Windows libkrun FFI runtime backend
//!
//! Minimal implementation for Windows that uses `std::process::Command` to
//! spawn a subprocess calling krun.dll's C API. Unlike the Unix version which
//! uses fork(), this uses Windows process creation.
//!
//! On Windows, WHPX (Windows Hypervisor Platform) is used instead of KVM/HVF.
//! gvproxy is not available — TSI networking is the only option.

use super::ffi;
use super::gvproxy::{GvproxyInstance, GvproxyManager};
use super::ExecOutput;
use crate::config::{NetworkScope, SandboxConfig};
use crate::error::{Error, Result};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Configuration passed to the `internal-boot-vm` subprocess.
#[derive(Serialize, Deserialize)]
pub struct BootVmRequest {
    pub sandbox_id: String,
    pub rootfs_path: String,
    pub cpus: u32,
    pub memory_mb: u32,
    pub command: String,
    pub command_args: Vec<String>,
    pub network_scope: NetworkScope,
    pub port_mappings: Vec<(u16, u16, String)>,
    pub mounts: Vec<(String, String)>,
    pub dns: Vec<String>,
    pub gvproxy_socket: Option<String>,
}

/// Entry point for the `internal-boot-vm` subprocess on Windows.
pub fn handle_boot_vm_subprocess() -> ! {
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("internal-boot-vm: failed to read config from stdin: {}", e);
        std::process::exit(1);
    }

    let config: BootVmRequest = match serde_json::from_str(&input) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("internal-boot-vm: failed to parse config JSON: {}", e);
            std::process::exit(1);
        }
    };

    let _ = ffi::init_log(ffi::KRUN_LOG_TARGET_DEFAULT, ffi::KRUN_LOG_LEVEL_WARN);

    let mut env = HashMap::new();
    env.insert(
        "PATH".to_string(),
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
    );
    env.insert("HOME".to_string(), "/root".to_string());
    env.insert("TERM".to_string(), "dumb".to_string());
    if !config.dns.is_empty() {
        env.insert("NANOSANDBOX_DNS".to_string(), config.dns.join(","));
    }

    let args_refs: Vec<&str> = config.command_args.iter().map(|s| s.as_str()).collect();

    let result = LibkrunRuntime::configure_and_start_vm(
        &config.rootfs_path,
        config.cpus,
        config.memory_mb,
        &config.command,
        &args_refs,
        None,
        &env,
        &config.network_scope,
        &config.port_mappings,
        &config.mounts,
        &config.dns,
        config.gvproxy_socket.as_deref(),
    );

    if let Err(e) = result {
        eprintln!("internal-boot-vm: configure_and_start_vm failed: {}", e);
    }
    std::process::exit(1);
}

/// State tracked per sandbox
struct SandboxState {
    rootfs_path: PathBuf,
    cpus: u32,
    memory_mb: u32,
    dns: Vec<String>,
    network_scope: NetworkScope,
    port_mappings: Vec<(u16, u16, String)>,
    mounts: Vec<(String, String)>,
    gvproxy: Option<GvproxyInstance>,
    gateway_port: Option<u16>,
    vm_child: Option<std::process::Child>,
    has_gateway: bool,
    mcp_servers: HashMap<String, McpServerConfig>,
    resolved_agent: Option<ResolvedAgentConfig>,
    vm_log_path: Option<PathBuf>,
    ssh_key_path: Option<PathBuf>,
    ssh_port: Option<u16>,
}

/// Direct libkrun FFI runtime for Windows (WHPX)
///
/// Uses `std::process::Command` to spawn VM subprocess (no fork on Windows).
/// TSI networking only (no gvproxy).
pub struct LibkrunRuntime {
    sandboxes: std::sync::Mutex<HashMap<String, SandboxState>>,
}

impl LibkrunRuntime {
    pub async fn new() -> Result<Self> {
        Ok(Self {
            sandboxes: std::sync::Mutex::new(HashMap::new()),
        })
    }

    pub fn is_available() -> bool {
        ffi::create_ctx().is_ok()
    }

    pub fn handles_image_pull(&self) -> bool {
        false
    }

    pub fn is_persistent(&self, id: &str) -> bool {
        let sandboxes = self.sandboxes.lock().unwrap();
        sandboxes.get(id).map_or(false, |s| s.has_gateway)
    }

    pub fn ssh_port(&self, id: &str) -> Option<u16> {
        let sandboxes = self.sandboxes.lock().unwrap();
        sandboxes.get(id).and_then(|s| s.ssh_port)
    }

    pub fn ssh_key_path(&self, id: &str) -> Option<PathBuf> {
        let sandboxes = self.sandboxes.lock().unwrap();
        sandboxes.get(id).and_then(|s| s.ssh_key_path.clone())
    }

    pub fn expose_port(&self, _id: &str, _port: u16) -> std::result::Result<(), String> {
        Err("expose_port not available on Windows (no gvproxy)".into())
    }

    pub fn ssh_command(&self, id: &str) -> Option<String> {
        let port = self.ssh_port(id)?;
        let key_path = self.ssh_key_path(id)?;
        Some(format!(
            "ssh -i {} -p {} -o StrictHostKeyChecking=no -o UserKnownHostsFile=NUL root@127.0.0.1",
            key_path.display(),
            port
        ))
    }

    /// Configure and start a libkrun VM (called in subprocess context).
    fn configure_and_start_vm(
        rootfs_path: &str,
        cpus: u32,
        memory_mb: u32,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
        network_scope: &NetworkScope,
        port_mappings: &[(u16, u16, String)],
        mounts: &[(String, String)],
        dns: &[String],
        _gvproxy_socket: Option<&str>,
    ) -> std::result::Result<(), String> {
        let ctx_id = ffi::create_ctx()?;

        ffi::set_vm_config(ctx_id, cpus.min(255) as u8, memory_mb)?;
        ffi::set_root(ctx_id, rootfs_path)?;

        if let Some(wd) = workdir {
            let _ = ffi::set_workdir(ctx_id, wd);
        }

        // Environment
        let env_strings: Vec<String> = env.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
        ffi::set_env(ctx_id, &env_strings)?;

        // Exec
        ffi::set_exec(ctx_id, command, args, None)?;

        // Mounts
        for (host, container) in mounts {
            let _ = ffi::add_virtiofs(ctx_id, container, host);
        }

        // TSI networking (always, since no gvproxy on Windows)
        let tsi_features = match network_scope {
            NetworkScope::Any => ffi::KRUN_TSI_HIJACK_INET | ffi::KRUN_TSI_HIJACK_UNIX,
            NetworkScope::Public | NetworkScope::Group => ffi::KRUN_TSI_HIJACK_INET,
            NetworkScope::None => 0,
        };
        if tsi_features > 0 {
            let _ = ffi::add_vsock(ctx_id, tsi_features);
        }

        // Port mappings
        if !port_mappings.is_empty() {
            let mapping_strings: Vec<String> = port_mappings
                .iter()
                .map(|(h, g, _proto)| format!("{}:{}", h, g))
                .collect();
            let _ = ffi::set_port_map(ctx_id, Some(&mapping_strings));
        }

        // Start VM (never returns on success)
        ffi::start_enter(ctx_id)?;
        Ok(())
    }

    /// Launch VM in a subprocess using std::process::Command
    fn launch_vm_subprocess(
        &self,
        sandbox_id: &str,
        rootfs_path: &str,
        cpus: u32,
        memory_mb: u32,
        command: &str,
        args: &[&str],
        network_scope: &NetworkScope,
        port_mappings: &[(u16, u16, String)],
        mounts: &[(String, String)],
        dns: &[String],
        gvproxy_socket: Option<&str>,
    ) -> std::result::Result<std::process::Child, String> {
        let request = BootVmRequest {
            sandbox_id: sandbox_id.to_string(),
            rootfs_path: rootfs_path.to_string(),
            cpus,
            memory_mb,
            command: command.to_string(),
            command_args: args.iter().map(|s| s.to_string()).collect(),
            network_scope: network_scope.clone(),
            port_mappings: port_mappings.to_vec(),
            mounts: mounts.to_vec(),
            dns: dns.to_vec(),
            gvproxy_socket: gvproxy_socket.map(|s| s.to_string()),
        };

        let json = serde_json::to_string(&request)
            .map_err(|e| format!("Failed to serialize BootVmRequest: {}", e))?;

        let exe =
            std::env::current_exe().map_err(|e| format!("Failed to get current exe: {}", e))?;

        let mut child = std::process::Command::new(&exe)
            .arg("internal-boot-vm")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn VM subprocess: {}", e))?;

        // Write config to stdin
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(json.as_bytes());
        }

        Ok(child)
    }

    pub async fn create(
        &self,
        id: &str,
        config: &SandboxConfig,
        bundle_path: Option<&Path>,
    ) -> Result<()> {
        let rootfs_path = bundle_path
            .map(|p| p.join("rootfs"))
            .unwrap_or_else(|| PathBuf::from("/tmp/nanosb-rootfs"));

        let has_gateway = rootfs_path.join("usr/local/bin/agent-gateway").exists();

        let state = SandboxState {
            rootfs_path,
            cpus: config.cpus,
            memory_mb: config.memory_mb,
            dns: config.network.dns.clone(),
            network_scope: config.network.scope.clone(),
            port_mappings: config
                .network
                .port_mappings
                .iter()
                .map(|pm| (pm.host_port, pm.container_port, pm.protocol.clone()))
                .collect(),
            mounts: config
                .mounts
                .iter()
                .map(|m| {
                    (
                        m.host_path.to_string_lossy().to_string(),
                        m.container_path.clone(),
                    )
                })
                .collect(),
            gvproxy: None,
            gateway_port: None,
            vm_child: None,
            has_gateway,
            mcp_servers: HashMap::new(),
            resolved_agent: None,
            vm_log_path: None,
            ssh_key_path: None,
            ssh_port: None,
        };

        self.sandboxes.lock().unwrap().insert(id.to_string(), state);
        info!("Created sandbox '{}' (Windows/WHPX)", id);
        Ok(())
    }

    pub async fn start(&self, id: &str) -> Result<()> {
        let (rootfs_path, cpus, memory_mb, network_scope, port_mappings, mounts, dns) = {
            let sandboxes = self.sandboxes.lock().unwrap();
            let state = sandboxes
                .get(id)
                .ok_or_else(|| Error::SandboxNotFound(id.to_string()))?;
            (
                state.rootfs_path.to_string_lossy().to_string(),
                state.cpus,
                state.memory_mb,
                state.network_scope.clone(),
                state.port_mappings.clone(),
                state.mounts.clone(),
                state.dns.clone(),
            )
        };

        let command = "/bin/sh";
        let args = &["sh", "/usr/local/bin/nanosb-init.sh"];

        let child = self
            .launch_vm_subprocess(
                id,
                &rootfs_path,
                cpus,
                memory_mb,
                command,
                args,
                &network_scope,
                &port_mappings,
                &mounts,
                &dns,
                None,
            )
            .map_err(|e| Error::SandboxCreationFailed(e))?;

        {
            let mut sandboxes = self.sandboxes.lock().unwrap();
            if let Some(state) = sandboxes.get_mut(id) {
                state.vm_child = Some(child);
            }
        }

        info!("Started sandbox '{}' (Windows/WHPX subprocess)", id);
        Ok(())
    }

    pub async fn exec(
        &self,
        id: &str,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
    ) -> Result<ExecOutput> {
        // For exec, we launch a new VM subprocess with the given command
        let (rootfs_path, cpus, memory_mb, network_scope, port_mappings, mounts, dns) = {
            let sandboxes = self.sandboxes.lock().unwrap();
            let state = sandboxes
                .get(id)
                .ok_or_else(|| Error::SandboxNotFound(id.to_string()))?;
            (
                state.rootfs_path.to_string_lossy().to_string(),
                state.cpus,
                state.memory_mb,
                state.network_scope.clone(),
                state.port_mappings.clone(),
                state.mounts.clone(),
                state.dns.clone(),
            )
        };

        let mut child = self
            .launch_vm_subprocess(
                id,
                &rootfs_path,
                cpus,
                memory_mb,
                command,
                args,
                &network_scope,
                &port_mappings,
                &mounts,
                &dns,
                None,
            )
            .map_err(|e| Error::ExecFailed(e))?;

        let stdout = child
            .stdout
            .take()
            .map(|s| {
                let mut reader = BufReader::new(s);
                let mut content = String::new();
                let _ = reader.read_to_string(&mut content);
                content
            })
            .unwrap_or_default();

        let stderr = child
            .stderr
            .take()
            .map(|s| {
                let mut reader = BufReader::new(s);
                let mut content = String::new();
                let _ = reader.read_to_string(&mut content);
                content
            })
            .unwrap_or_default();

        let status = child.wait().map_err(|e| Error::ExecFailed(e.to_string()))?;

        Ok(ExecOutput {
            exit_code: status.code().unwrap_or(-1),
            stdout,
            stderr,
        })
    }

    pub async fn exec_stream<F>(
        &self,
        id: &str,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
        on_output: F,
    ) -> Result<i32>
    where
        F: Fn(&str, bool) + Send + Sync,
    {
        let result = self.exec(id, command, args, workdir, env).await?;
        if !result.stdout.is_empty() {
            on_output(&result.stdout, false);
        }
        if !result.stderr.is_empty() {
            on_output(&result.stderr, true);
        }
        Ok(result.exit_code)
    }

    /// Send a generic HTTP GET to the gateway process inside a sandbox.
    pub fn gateway_http_get(&self, _id: &str, _path: &str) -> Result<(u16, String)> {
        Err(Error::ExecFailed(
            "Gateway HTTP not yet implemented on Windows".to_string(),
        ))
    }

    /// Send a generic HTTP POST to the gateway process inside a sandbox.
    pub fn gateway_http_post(&self, _id: &str, _path: &str, _json_body: &str) -> Result<(u16, String)> {
        Err(Error::ExecFailed(
            "Gateway HTTP not yet implemented on Windows".to_string(),
        ))
    }

    /// Send a generic HTTP DELETE to the gateway process inside a sandbox.
    pub fn gateway_http_delete(&self, _id: &str, _path: &str) -> Result<(u16, String)> {
        Err(Error::ExecFailed(
            "Gateway HTTP not yet implemented on Windows".to_string(),
        ))
    }

    /// Send a generic HTTP POST with SSE streaming to the gateway process inside a sandbox.
    pub fn gateway_http_post_sse<F>(&self, _id: &str, _path: &str, _json_body: &str, _on_output: F) -> Result<i32>
    where
        F: Fn(&str, bool) + Send + Sync,
    {
        Err(Error::ExecFailed(
            "Gateway HTTP SSE not yet implemented on Windows".to_string(),
        ))
    }

    pub async fn stop(&self, id: &str) -> Result<()> {
        let mut sandboxes = self.sandboxes.lock().unwrap();
        if let Some(state) = sandboxes.get_mut(id) {
            if let Some(ref mut child) = state.vm_child {
                let _ = child.kill();
                let _ = child.wait();
            }
            state.vm_child = None;
        }
        info!("Stopped sandbox '{}'", id);
        Ok(())
    }

    pub async fn destroy(&self, id: &str) -> Result<()> {
        self.stop(id).await?;
        self.sandboxes.lock().unwrap().remove(id);
        info!("Destroyed sandbox '{}'", id);
        Ok(())
    }

}
