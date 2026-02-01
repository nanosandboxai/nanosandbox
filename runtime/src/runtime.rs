//! Runtime integration with crun/libkrun

use crate::error::{Error, Result};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

/// Runtime for executing sandboxes
pub struct Runtime {
    /// Path to crun/krun binary
    binary_path: String,
    /// Runtime root directory
    root_dir: PathBuf,
}

impl Runtime {
    /// Create a new runtime
    pub async fn new() -> Result<Self> {
        let binary_path = Self::find_runtime().await?;
        let root_dir = Self::default_root_dir()?;

        Ok(Self {
            binary_path,
            root_dir,
        })
    }

    /// Find the crun/krun binary
    async fn find_runtime() -> Result<String> {
        // First try krun (crun with libkrun)
        for candidate in &["krun", "crun"] {
            let output = Command::new("which")
                .arg(candidate)
                .output()
                .await;

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
            "Neither 'krun' nor 'crun' found. Please install crun with libkrun support.".to_string(),
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

    /// Get the runtime binary path
    pub fn binary_path(&self) -> &str {
        &self.binary_path
    }

    /// Get the root directory
    pub fn root_dir(&self) -> &PathBuf {
        &self.root_dir
    }

    /// Create a container/VM
    pub async fn create(&self, id: &str, bundle_path: &PathBuf) -> Result<()> {
        let output = Command::new(&self.binary_path)
            .args([
                "--root",
                self.root_dir.to_str().unwrap(),
                "create",
                "--bundle",
                bundle_path.to_str().unwrap(),
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

    /// Start a container/VM
    pub async fn start(&self, id: &str) -> Result<()> {
        let output = Command::new(&self.binary_path)
            .args([
                "--root",
                self.root_dir.to_str().unwrap(),
                "start",
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

    /// Execute a command in a running container/VM
    pub async fn exec(
        &self,
        id: &str,
        command: &str,
        args: &[&str],
    ) -> Result<std::process::Output> {
        let mut exec_args = vec![
            "--root",
            self.root_dir.to_str().unwrap(),
            "exec",
            id,
            command,
        ];
        exec_args.extend(args);

        let output = Command::new(&self.binary_path)
            .args(&exec_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        Ok(output)
    }

    /// Stop a container/VM
    pub async fn kill(&self, id: &str) -> Result<()> {
        let output = Command::new(&self.binary_path)
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
            .await?;

        if output.status.success() {
            Ok(())
        } else {
            // Ignore errors if container already stopped
            Ok(())
        }
    }

    /// Delete a container/VM
    pub async fn delete(&self, id: &str) -> Result<()> {
        let output = Command::new(&self.binary_path)
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
            .await?;

        if output.status.success() {
            Ok(())
        } else {
            // Ignore errors if container doesn't exist
            Ok(())
        }
    }

    /// Get container/VM state
    pub async fn state(&self, id: &str) -> Result<String> {
        let output = Command::new(&self.binary_path)
            .args([
                "--root",
                self.root_dir.to_str().unwrap(),
                "state",
                id,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(Error::SandboxNotFound(id.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_runtime_creation() {
        match Runtime::new().await {
            Ok(runtime) => {
                assert!(!runtime.binary_path().is_empty());
            }
            Err(e) => {
                eprintln!("Runtime not available for testing: {}", e);
            }
        }
    }
}
