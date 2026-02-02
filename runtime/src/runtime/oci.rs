//! OCI Runtime Backend (crun/krun)
//!
//! This backend uses crun or krun for container execution.
//! It follows the OCI runtime specification.

use super::ExecOutput;
use crate::config::SandboxConfig;
use crate::error::{Error, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::debug;

/// OCI Runtime (crun/krun)
pub struct OciRuntime {
    /// Path to the runtime binary
    binary_path: String,
    /// Runtime root directory for state
    root_dir: PathBuf,
}

impl OciRuntime {
    /// Create a new OCI runtime
    pub async fn new() -> Result<Self> {
        let binary_path = Self::find_binary().await?;
        let root_dir = Self::default_root_dir()?;

        Ok(Self {
            binary_path,
            root_dir,
        })
    }

    /// Find the OCI runtime binary
    async fn find_binary() -> Result<String> {
        for candidate in &["krun", "crun"] {
            let output = Command::new("which").arg(candidate).output().await;

            if let Ok(out) = output {
                if out.status.success() {
                    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !path.is_empty() {
                        return Ok(path);
                    }
                }
            }
        }

        Err(Error::RuntimeNotAvailable(
            "Neither 'krun' nor 'crun' found".to_string(),
        ))
    }

    /// Get default root directory for runtime state
    fn default_root_dir() -> Result<PathBuf> {
        let root = dirs::runtime_dir()
            .or_else(|| Some(PathBuf::from("/tmp")))
            .unwrap()
            .join("nanosandbox")
            .join("runtime");

        std::fs::create_dir_all(&root)?;
        Ok(root)
    }

    /// Get the binary path
    pub fn binary_path(&self) -> &str {
        &self.binary_path
    }
}

impl OciRuntime {
    /// Get the runtime name
    pub fn name(&self) -> &str {
        if self.binary_path.contains("krun") {
            "krun"
        } else {
            "crun"
        }
    }

    /// Check if OCI runtime is available
    pub async fn is_available() -> bool {
        Self::find_binary().await.is_ok()
    }

    /// Create a container from OCI bundle
    pub async fn create(
        &self,
        id: &str,
        _config: &SandboxConfig,
        bundle_path: Option<&Path>,
    ) -> Result<()> {
        let bundle = bundle_path.ok_or_else(|| {
            Error::SandboxCreationFailed("OCI runtime requires bundle path".to_string())
        })?;

        let output = Command::new(&self.binary_path)
            .args([
                "--root",
                self.root_dir.to_str().unwrap(),
                "create",
                "--bundle",
                bundle.to_str().unwrap(),
                id,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if output.status.success() {
            Ok(())
        } else {
            Err(Error::SandboxCreationFailed(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ))
        }
    }

    /// Start a container
    pub async fn start(&self, id: &str) -> Result<()> {
        let output = Command::new(&self.binary_path)
            .args(["--root", self.root_dir.to_str().unwrap(), "start", id])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if output.status.success() {
            Ok(())
        } else {
            Err(Error::SandboxCreationFailed(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ))
        }
    }

    /// Execute a command in a running container
    pub async fn exec(
        &self,
        id: &str,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
    ) -> Result<ExecOutput> {
        let mut exec_args: Vec<String> = vec![
            "--root".to_string(),
            self.root_dir.to_str().unwrap().to_string(),
            "exec".to_string(),
        ];

        if let Some(cwd) = workdir {
            exec_args.push("--cwd".to_string());
            exec_args.push(cwd.to_string());
        }

        for (key, value) in env {
            exec_args.push("--env".to_string());
            exec_args.push(format!("{}={}", key, value));
        }

        exec_args.push(id.to_string());
        exec_args.push(command.to_string());
        exec_args.extend(args.iter().map(|s| s.to_string()));

        debug!("Executing: {} {:?}", self.binary_path, exec_args);

        let output = Command::new(&self.binary_path)
            .args(&exec_args)
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

    /// Execute a command with streaming output
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
        let mut exec_args: Vec<String> = vec![
            "--root".to_string(),
            self.root_dir.to_str().unwrap().to_string(),
            "exec".to_string(),
        ];

        if let Some(cwd) = workdir {
            exec_args.push("--cwd".to_string());
            exec_args.push(cwd.to_string());
        }

        for (key, value) in env {
            exec_args.push("--env".to_string());
            exec_args.push(format!("{}={}", key, value));
        }

        exec_args.push(id.to_string());
        exec_args.push(command.to_string());
        exec_args.extend(args.iter().map(|s| s.to_string()));

        debug!(
            "Executing (streaming): {} {:?}",
            self.binary_path, exec_args
        );

        let mut child = Command::new(&self.binary_path)
            .args(&exec_args)
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

    /// Stop a container
    pub async fn stop(&self, id: &str) -> Result<()> {
        let _ = Command::new(&self.binary_path)
            .args([
                "--root",
                self.root_dir.to_str().unwrap(),
                "kill",
                id,
                "SIGTERM",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        Ok(())
    }

    /// Destroy a container
    pub async fn destroy(&self, id: &str) -> Result<()> {
        let _ = Command::new(&self.binary_path)
            .args([
                "--root",
                self.root_dir.to_str().unwrap(),
                "delete",
                "--force",
                id,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        Ok(())
    }
}
