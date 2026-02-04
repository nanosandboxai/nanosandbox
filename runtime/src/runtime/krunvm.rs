//! krunvm Runtime Backend for macOS
//!
//! This backend uses krunvm for VM-based sandbox execution on macOS Apple Silicon.
//! krunvm manages microVMs using libkrun and Hypervisor.framework.

use super::ExecOutput;
use crate::config::SandboxConfig;
use crate::error::{Error, Result};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{debug, info};

/// krunvm Runtime for macOS Apple Silicon
///
/// krunvm has a different model than OCI runtimes:
/// - `create`: Creates a VM from an OCI image (handles image pull internally)
/// - `start`: Starts the VM and runs a command (then VM exits)
/// - No persistent "running" state like containers
///
/// This means each `exec` is actually a `krunvm start` with the command.
pub struct KrunVmRuntime {
    /// Path to the krunvm binary
    binary_path: String,
}

impl KrunVmRuntime {
    /// Create a new krunvm runtime
    pub async fn new() -> Result<Self> {
        let binary_path = Self::find_binary().await?;
        Ok(Self { binary_path })
    }

    /// Find the krunvm binary
    async fn find_binary() -> Result<String> {
        let output = Command::new("which").arg("krunvm").output().await;

        if let Ok(out) = output {
            if out.status.success() {
                let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !path.is_empty() {
                    return Ok(path);
                }
            }
        }

        Err(Error::RuntimeNotAvailable(
            "krunvm not found. Install with: brew tap slp/krun && brew install krunvm".to_string(),
        ))
    }

    /// Check if a VM with the given name exists
    async fn vm_exists(&self, id: &str) -> bool {
        let output = Command::new(&self.binary_path)
            .args(["list"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            // krunvm list output includes VM names
            return stdout.contains(id);
        }
        false
    }

    /// Delete VM if it exists (cleanup helper)
    async fn cleanup_vm(&self, id: &str) {
        if self.vm_exists(id).await {
            let _ = Command::new(&self.binary_path)
                .args(["delete", id])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await;
        }
    }
}

impl KrunVmRuntime {
    /// Check if krunvm is available
    pub async fn is_available() -> bool {
        Self::find_binary().await.is_ok()
    }

    /// Create a VM from OCI image
    pub async fn create(
        &self,
        id: &str,
        config: &SandboxConfig,
        _bundle_path: Option<&Path>,
    ) -> Result<()> {
        // Clean up any existing VM with this name
        self.cleanup_vm(id).await;

        // krunvm create <IMAGE> --name <id> --cpus N --mem M
        let output = Command::new(&self.binary_path)
            .args([
                "create",
                &config.image,
                "--name",
                id,
                "--cpus",
                &config.cpus.to_string(),
                "--mem",
                &config.memory_mb.to_string(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if output.status.success() {
            info!("Created krunvm VM: {} from image {}", id, config.image);
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(Error::SandboxCreationFailed(format!(
                "krunvm create failed: {}",
                stderr
            )))
        }
    }

    /// Start the VM (verify it exists)
    pub async fn start(&self, id: &str) -> Result<()> {
        // krunvm doesn't have a separate "start" - VMs are started with a command
        // For compatibility, we just verify the VM exists
        if self.vm_exists(id).await {
            Ok(())
        } else {
            Err(Error::SandboxNotFound(id.to_string()))
        }
    }

    /// Execute a command in the VM (boots VM, runs command, exits)
    pub async fn exec(
        &self,
        id: &str,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
    ) -> Result<ExecOutput> {
        // krunvm start <name> [command] [args]
        // Each start boots the VM, runs the command, and exits
        let mut cmd_args = vec!["start".to_string(), id.to_string()];

        // Add workdir if specified (krunvm uses -w)
        if let Some(cwd) = workdir {
            cmd_args.push("-w".to_string());
            cmd_args.push(cwd.to_string());
        }

        // Add environment variables (krunvm uses -e)
        for (key, value) in env {
            cmd_args.push("-e".to_string());
            cmd_args.push(format!("{}={}", key, value));
        }

        // Add separator to prevent krunvm from interpreting command flags as its own
        cmd_args.push("--".to_string());

        // Add command and args
        cmd_args.push(command.to_string());
        cmd_args.extend(args.iter().map(|s| s.to_string()));

        debug!("Executing: {} {:?}", self.binary_path, cmd_args);

        let output = Command::new(&self.binary_path)
            .args(&cmd_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        Ok(ExecOutput {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    /// Execute with streaming output
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
        let mut cmd_args = vec!["start".to_string(), id.to_string()];

        if let Some(cwd) = workdir {
            cmd_args.push("-w".to_string());
            cmd_args.push(cwd.to_string());
        }

        for (key, value) in env {
            cmd_args.push("-e".to_string());
            cmd_args.push(format!("{}={}", key, value));
        }

        // Add separator to prevent krunvm from interpreting command flags as its own
        cmd_args.push("--".to_string());

        cmd_args.push(command.to_string());
        cmd_args.extend(args.iter().map(|s| s.to_string()));

        debug!("Executing (streaming): {} {:?}", self.binary_path, cmd_args);

        let mut child = Command::new(&self.binary_path)
            .args(&cmd_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let mut stdout_reader = BufReader::new(stdout).lines();
        let mut stderr_reader = BufReader::new(stderr).lines();

        loop {
            tokio::select! {
                line = stdout_reader.next_line() => {
                    match line {
                        Ok(Some(text)) => on_output(&text, false),
                        Ok(None) => break,
                        Err(e) => {
                            debug!("Error reading stdout: {}", e);
                            break;
                        }
                    }
                }
                line = stderr_reader.next_line() => {
                    match line {
                        Ok(Some(text)) => on_output(&text, true),
                        Ok(None) => {},
                        Err(e) => {
                            debug!("Error reading stderr: {}", e);
                        }
                    }
                }
            }
        }

        while let Ok(Some(text)) = stderr_reader.next_line().await {
            on_output(&text, true);
        }

        let status = child.wait().await?;
        Ok(status.code().unwrap_or(-1))
    }

    /// Stop the VM (no-op for krunvm)
    pub async fn stop(&self, _id: &str) -> Result<()> {
        // krunvm VMs exit after command completes, no explicit stop needed
        Ok(())
    }

    /// Destroy the VM
    pub async fn destroy(&self, id: &str) -> Result<()> {
        // krunvm delete <name>
        let output = Command::new(&self.binary_path)
            .args(["delete", id])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if output.status.success() {
            info!("Deleted krunvm VM: {}", id);
            Ok(())
        } else {
            // Ignore errors (VM may not exist)
            debug!(
                "krunvm delete warning: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(())
        }
    }
}
