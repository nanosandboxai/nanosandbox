//! GatewayClient — high-level API for communicating with agent-gateway inside a VM.

use crate::error::{Error, Result};
use crate::secrets::{self, GatewayKeyPair, SecretManifest, SecretPayload};
use crate::transport::Transport;
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Result of executing a command via the gateway.
#[derive(Debug, Clone)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

impl ExecResult {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Options for command execution.
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    pub workdir: Option<String>,
    pub env: HashMap<String, String>,
    pub user: Option<String>,
    pub timeout_secs: Option<u32>,
}

impl ExecOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn workdir(mut self, workdir: &str) -> Self {
        self.workdir = Some(workdir.to_string());
        self
    }

    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.insert(key.to_string(), value.to_string());
        self
    }

    pub fn user(mut self, user: &str) -> Self {
        self.user = Some(user.to_string());
        self
    }

    pub fn timeout(mut self, secs: u32) -> Self {
        self.timeout_secs = Some(secs);
        self
    }
}

/// Output chunk from streaming exec.
#[derive(Debug, Clone)]
pub struct OutputChunk {
    pub stream: Stream,
    pub data: String,
    pub timestamp: chrono::DateTime<Utc>,
}

/// Stream identifier for output chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// Client for communicating with the agent-gateway process inside a VM.
pub struct GatewayClient {
    transport: Transport,
    secrets_keypair: Option<GatewayKeyPair>,
    secrets_store: HashMap<String, String>,
    default_timeout_secs: u32,
    config_env: HashMap<String, String>,
    sandbox_id: String,
}

impl GatewayClient {
    pub fn new(
        gateway_addr: Option<String>,
        sandbox_id: String,
        default_timeout_secs: u32,
        config_env: HashMap<String, String>,
    ) -> Self {
        Self {
            transport: Transport::new(gateway_addr),
            secrets_keypair: None,
            secrets_store: HashMap::new(),
            default_timeout_secs,
            config_env,
            sandbox_id,
        }
    }

    pub fn generate_secrets_keypair(&mut self) {
        self.secrets_keypair = Some(GatewayKeyPair::generate());
        debug!("Generated ephemeral secrets keypair for gateway");
    }

    pub fn secrets_pubkey(&self) -> Option<String> {
        self.secrets_keypair.as_ref().map(|kp| kp.public_key_base64())
    }

    pub fn is_available(&self) -> bool {
        self.transport.is_available()
    }

    // -- HTTP convenience methods --

    pub fn http_get(&self, path: &str) -> Result<(u16, String)> {
        self.transport.http_get(path)
    }

    pub fn http_post(&self, path: &str, json_body: &str) -> Result<(u16, String)> {
        self.transport.http_post(path, json_body)
    }

    pub fn http_delete(&self, path: &str) -> Result<(u16, String)> {
        self.transport.http_delete(path)
    }

    pub fn http_post_sse<F>(&self, path: &str, json_body: &str, on_output: F) -> Result<i32>
    where
        F: Fn(&str, bool) + Send + Sync,
    {
        self.transport.http_post_sse(path, json_body, on_output)
    }

    // -- Exec --

    fn exec_body(
        command: &str,
        args: &[&str],
        env: &HashMap<String, String>,
        timeout_secs: u32,
    ) -> String {
        serde_json::json!({
            "command": command,
            "args": args,
            "env": env,
            "timeout": timeout_secs,
        })
        .to_string()
    }

    fn merged_env(&self, exec_env: HashMap<String, String>) -> HashMap<String, String> {
        let mut merged = self.config_env.clone();
        merged.extend(exec_env);
        merged.extend(self.secrets_store.clone());
        merged
    }

    pub async fn exec(&self, command: &str, args: &[&str]) -> Result<ExecResult> {
        self.exec_with_options(command, args, ExecOptions::default()).await
    }

    pub async fn exec_with_options(
        &self,
        command: &str,
        args: &[&str],
        options: ExecOptions,
    ) -> Result<ExecResult> {
        let timeout_secs = options.timeout_secs.unwrap_or(self.default_timeout_secs);
        let merged_env = self.merged_env(options.env);
        let start = std::time::Instant::now();

        debug!("Executing via gateway: {} {:?}", command, args);
        let json_body = Self::exec_body(command, args, &merged_env, timeout_secs);
        let transport = self.clone_transport();

        let result = tokio::task::spawn_blocking(move || {
            let stdout = std::sync::Mutex::new(String::new());
            let stderr = std::sync::Mutex::new(String::new());

            let on_output = |text: &str, is_stderr: bool| {
                if is_stderr {
                    stderr.lock().unwrap().push_str(text);
                } else {
                    stdout.lock().unwrap().push_str(text);
                }
            };

            let exit_code = transport
                .http_post_sse("/api/v1/exec", &json_body, on_output)
                .map_err(|e| Error::ExecFailed(format!("Gateway exec failed: {}", e)))?;

            Ok::<_, Error>(ExecResult {
                exit_code,
                stdout: stdout.into_inner().unwrap(),
                stderr: stderr.into_inner().unwrap(),
                duration_ms: 0,
            })
        })
        .await
        .map_err(|e| Error::ExecFailed(format!("Gateway exec task failed: {}", e)))??;

        Ok(ExecResult {
            duration_ms: start.elapsed().as_millis() as u64,
            ..result
        })
    }

    pub async fn exec_stream<F>(&self, command: &str, args: &[&str], on_output: F) -> Result<i32>
    where
        F: Fn(OutputChunk) + Send + Sync + 'static,
    {
        self.exec_stream_with_options(command, args, ExecOptions::default(), on_output)
            .await
    }

    pub async fn exec_stream_with_options<F>(
        &self,
        command: &str,
        args: &[&str],
        options: ExecOptions,
        on_output: F,
    ) -> Result<i32>
    where
        F: Fn(OutputChunk) + Send + Sync + 'static,
    {
        let timeout_secs = options.timeout_secs.unwrap_or(self.default_timeout_secs);
        let merged_env = self.merged_env(options.env);

        let env_keys: Vec<&str> = merged_env.keys().map(|s| s.as_str()).collect();
        let prompt_present = merged_env
            .get("NANOSB_PROMPT")
            .map(|v| format!("len={}", v.len()))
            .unwrap_or_else(|| "MISSING".to_string());
        info!(
            "[gateway_exec] command={} args={:?} env_keys={:?} NANOSB_PROMPT={}",
            command, args, env_keys, prompt_present,
        );

        let json_body = Self::exec_body(command, args, &merged_env, timeout_secs);
        let on_output = Arc::new(on_output);
        let transport = self.clone_transport();

        let on_output_blocking = on_output.clone();
        let exit_code = tokio::task::spawn_blocking(move || {
            let on_sse = |text: &str, is_stderr: bool| {
                on_output_blocking(OutputChunk {
                    stream: if is_stderr { Stream::Stderr } else { Stream::Stdout },
                    data: text.to_string(),
                    timestamp: Utc::now(),
                });
            };
            transport
                .http_post_sse("/api/v1/exec", &json_body, on_sse)
                .map_err(|e| Error::ExecFailed(format!("Gateway exec failed: {}", e)))
        })
        .await
        .map_err(|e| Error::ExecFailed(format!("Gateway exec task join failed: {}", e)))??;

        Ok(exit_code)
    }

    // -- Health check --

    pub async fn wait_for_health<F>(&self, is_vm_running: F) -> Result<()>
    where
        F: Fn() -> bool,
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(240);
        let start = std::time::Instant::now();

        info!("Waiting for gateway health...");

        loop {
            if std::time::Instant::now() > deadline {
                error!("Gateway health check timed out after 240s");
                return Err(Error::HealthTimeout(240));
            }

            if !is_vm_running() {
                let elapsed = start.elapsed().as_secs_f32();
                error!("VM process died after {:.1}s — aborting health check", elapsed);
                return Err(Error::VmDied);
            }

            match self.transport.http_get("/health") {
                Ok((status, _)) if status == 200 => {
                    let elapsed = start.elapsed().as_secs_f32();
                    info!("Gateway health check passed in {:.1}s", elapsed);
                    return Ok(());
                }
                _ => {
                    debug!("Gateway not reachable yet, retrying...");
                }
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    // -- Secrets --

    pub fn send_secrets(&mut self, encrypted_json: &str) -> Result<SecretManifest> {
        let kp = self
            .secrets_keypair
            .as_mut()
            .ok_or_else(|| Error::SecretsError(
                "secrets keypair not available (gateway not started?)".to_string(),
            ))?;

        let encrypted: secrets::EncryptedPayload = serde_json::from_str(encrypted_json)
            .map_err(|e| Error::SecretsError(format!("failed to parse EncryptedPayload: {e}")))?;

        let payload = kp
            .decrypt(&encrypted)
            .map_err(Error::SecretsError)?;

        for (key, value) in &payload.secrets {
            self.secrets_store.insert(key.clone(), value.clone());
        }

        // POST all env vars to gateway SSH env store
        {
            let mut all_env = self.config_env.clone();
            for (key, value) in &payload.secrets {
                all_env.insert(key.clone(), value.clone());
            }

            if !all_env.is_empty() {
                let env_json = serde_json::to_string(&all_env)
                    .map_err(|e| Error::SecretsError(format!("failed to serialize env: {e}")))?;

                let (status, _) = self
                    .transport
                    .http_post("/api/v1/secrets/env", &env_json)
                    .map_err(|e| Error::SecretsError(format!("POST secrets/env failed: {e}")))?;

                if status >= 400 {
                    warn!("POST /api/v1/secrets/env returned HTTP {}", status);
                }

                debug!(
                    "Posted {} env vars ({} secrets + {} config) to gateway SSH env store",
                    all_env.len(),
                    payload.secrets.len(),
                    self.config_env.len()
                );
            }
        }

        let (script, manifest) = Self::generate_secrets_writer_script(&payload, &self.sandbox_id);

        if script.is_empty() {
            return Ok(manifest);
        }

        let json_body = Self::exec_body("sh", &["-c", &script], &HashMap::new(), 30);
        let (status, body) = self
            .transport
            .http_post("/api/v1/exec", &json_body)
            .map_err(|e| Error::SecretsError(format!("send_secrets exec failed: {e}")))?;

        if status != 200 {
            return Err(Error::SecretsError(format!(
                "send_secrets exec returned HTTP {}: {}",
                status, body
            )));
        }

        Ok(manifest)
    }

    fn generate_secrets_writer_script(
        payload: &SecretPayload,
        session_id: &str,
    ) -> (String, SecretManifest) {
        let sq_escape = |v: &str| v.replace('\'', "'\\''");

        let mut cmds: Vec<String> = Vec::new();
        let mut manifest_secrets = HashMap::new();
        let mut manifest_files = HashMap::new();

        for key in payload.secrets.keys() {
            manifest_secrets.insert(key.clone(), format!("${{{}}} (env var)", key));
        }

        if !payload.intercepted_files.is_empty() {
            cmds.push("mkdir -p /run/secrets/files".to_string());
            cmds.push("chmod 700 /run/secrets /run/secrets/files".to_string());

            for (key, value) in &payload.intercepted_files {
                let hash = {
                    let input = format!("{session_id}:{key}");
                    let digest = Sha256::digest(input.as_bytes());
                    digest.iter().map(|b| format!("{b:02x}")).collect::<String>()
                };
                let path = format!("/run/secrets/files/{}", hash);
                let escaped = sq_escape(value);
                cmds.push(format!(
                    "printf '%s' '{}' > {} && chmod 400 {}",
                    escaped, path, path
                ));
                manifest_files.insert(key.clone(), path);
            }
        }

        let manifest = SecretManifest {
            secrets: manifest_secrets,
            files: manifest_files,
        };

        if cmds.is_empty() {
            return (String::new(), manifest);
        }

        (cmds.join(" && "), manifest)
    }

    // -- Stop --

    pub async fn stop(&self) -> Result<()> {
        info!("Sending stop request to gateway");
        let transport = self.clone_transport();
        let _ = tokio::task::spawn_blocking(move || {
            let _ = transport.http_post("/api/v1/stop", "{}");
        })
        .await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        Ok(())
    }

    // -- Internal helpers --

    /// Clone transport for use in spawn_blocking.
    fn clone_transport(&self) -> Transport {
        Transport::new(self.transport.gateway_addr().map(|s| s.to_string()))
    }
}
