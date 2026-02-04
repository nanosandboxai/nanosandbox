//! Windows Containerd Runtime Backend
//!
//! This backend uses containerd with the runhcs v2 shim for Windows container execution.
//! It leverages containerd's snapshot management and image handling via native gRPC API.
//!
//! # Architecture
//!
//! ```text
//! nanosandbox → containerd (gRPC) → containerd-shim-runhcs-v1 → HCS (vmcompute)
//! ```
//!
//! # Prerequisites
//!
//! - Windows 10/11 Pro, Enterprise, or Windows Server 2016+
//! - Containers feature enabled
//! - containerd installed and running
//! - containerd-shim-runhcs-v1.exe available (same directory as containerd.exe or in PATH)
//!
//! # Benefits over direct runhcs
//!
//! - containerd manages layer snapshots properly, avoiding ProcessBaseLayer idempotency issues
//! - Image pulling handled by containerd with proper caching
//! - Standard Runtime v2 shim protocol for container lifecycle
//! - Native gRPC communication (no CLI subprocess spawning)

use crate::config::SandboxConfig;
use crate::error::{Error, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{debug, info, warn};

use containerd_client::services::v1::{
    containers_client::ContainersClient,
    images_client::ImagesClient,
    snapshots::{snapshots_client::SnapshotsClient, MountsRequest, MountsResponse},
    tasks_client::TasksClient,
    transfer_client::TransferClient,
    Container, CreateContainerRequest, CreateTaskRequest, DeleteContainerRequest,
    DeleteTaskRequest, GetImageRequest, KillRequest, StartRequest, TransferOptions,
    TransferRequest,
};
use containerd_client::types::transfer::{ImageStore, OciRegistry, UnpackConfiguration};
use containerd_client::types::Platform;
use containerd_client::{connect, to_any, with_namespace};
use prost_types::Any;
use tonic::transport::Channel;
use tonic::Request;

/// Default containerd address on Windows (named pipe)
const DEFAULT_CONTAINERD_ADDRESS: &str = r"\\.\pipe\containerd-containerd";

/// Default namespace for nanosandbox containers
const DEFAULT_NAMESPACE: &str = "nanosandbox";

/// Runtime name for runhcs v2 shim
const RUNHCS_RUNTIME: &str = "io.containerd.runhcs.v1";

/// Windows containerd runtime using containerd gRPC API
pub struct ContainerdWindowsRuntime {
    /// gRPC channel to containerd
    channel: Channel,
    /// containerd address (named pipe path)
    address: String,
    /// Namespace for containers
    namespace: String,
    /// Isolation mode
    isolation: WindowsContainerdIsolation,
    /// State directory for runtime metadata
    state_dir: PathBuf,
}

/// Windows isolation mode for containerd
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowsContainerdIsolation {
    /// Process isolation - faster, shares kernel with host
    #[default]
    Process,
    /// Hyper-V isolation - each container runs in lightweight VM
    HyperV,
}

impl ContainerdWindowsRuntime {
    /// Create a new Windows containerd runtime
    ///
    /// This validates that containerd is reachable via gRPC.
    pub async fn new() -> Result<Self> {
        let address = std::env::var("CONTAINERD_ADDRESS")
            .unwrap_or_else(|_| DEFAULT_CONTAINERD_ADDRESS.to_string());

        let namespace = std::env::var("CONTAINERD_NAMESPACE")
            .unwrap_or_else(|_| DEFAULT_NAMESPACE.to_string());

        let state_dir = Self::default_state_dir()?;

        info!(
            "Initializing Windows containerd runtime (address: {}, namespace: {})",
            address, namespace
        );

        // Connect to containerd via gRPC
        let channel = connect(&address).await.map_err(|e| {
            Error::SandboxCreationFailed(format!(
                "Failed to connect to containerd at {}: {}. Is containerd running?",
                address, e
            ))
        })?;

        let runtime = Self {
            channel,
            address,
            namespace,
            isolation: WindowsContainerdIsolation::default(),
            state_dir,
        };

        // Verify connection by checking version
        runtime.verify_containerd().await?;

        Ok(runtime)
    }

    /// Create with specific isolation mode
    pub async fn with_isolation(isolation: WindowsContainerdIsolation) -> Result<Self> {
        let mut runtime = Self::new().await?;
        runtime.isolation = isolation;
        Ok(runtime)
    }

    /// Get default state directory
    fn default_state_dir() -> Result<PathBuf> {
        let state_dir = dirs::data_local_dir()
            .ok_or_else(|| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Cannot find local data directory",
                ))
            })?
            .join("nanosandbox")
            .join("containerd-state");

        std::fs::create_dir_all(&state_dir)?;
        Ok(state_dir)
    }

    /// Verify containerd is running and reachable via gRPC
    async fn verify_containerd(&self) -> Result<()> {
        use containerd_client::services::v1::version_client::VersionClient;

        let mut client = VersionClient::new(self.channel.clone());
        let response = client.version(()).await.map_err(|e| {
            Error::SandboxCreationFailed(format!(
                "containerd not reachable at {}: {}",
                self.address, e
            ))
        })?;

        let version = response.get_ref();
        debug!(
            "containerd connection verified (version: {}, revision: {})",
            version.version, version.revision
        );
        Ok(())
    }

    /// Get the runtime name
    pub fn name(&self) -> &str {
        "windows-containerd"
    }

    /// Get the isolation mode
    pub fn isolation(&self) -> WindowsContainerdIsolation {
        self.isolation
    }

    /// Pull an image using containerd Transfer API
    ///
    /// This is called automatically during container creation if the image isn't cached.
    pub async fn pull_image(&self, image: &str) -> Result<()> {
        info!("Pulling image via containerd gRPC: {}", image);

        let mut client = TransferClient::new(self.channel.clone());

        // Create the source (OCIRegistry)
        let source = OciRegistry {
            reference: image.to_string(),
            resolver: Default::default(),
        };

        // Windows platform
        let platform = Platform {
            os: "windows".to_string(),
            architecture: "amd64".to_string(),
            variant: "".to_string(),
            os_version: "".to_string(),
        };

        // Create the destination (ImageStore)
        let destination = ImageStore {
            name: image.to_string(),
            platforms: vec![platform.clone()],
            unpacks: vec![UnpackConfiguration {
                platform: Some(platform),
                snapshotter: "windows".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let request = TransferRequest {
            source: Some(to_any(&source)),
            destination: Some(to_any(&destination)),
            options: Some(TransferOptions::default()),
        };

        client
            .transfer(with_namespace!(request, &self.namespace))
            .await
            .map_err(|e| Error::ImagePullFailed(format!("containerd image pull failed: {}", e)))?;

        info!("Image pulled successfully: {}", image);
        Ok(())
    }

    /// Check if an image exists in containerd
    pub async fn image_exists(&self, image: &str) -> bool {
        let mut client = ImagesClient::new(self.channel.clone());

        let request = GetImageRequest {
            name: image.to_string(),
        };

        match client
            .get(with_namespace!(request, &self.namespace))
            .await
        {
            Ok(_) => true,
            Err(_) => false,
        }
    }

    /// Create a Windows container via containerd gRPC
    ///
    /// This will:
    /// 1. Pull the image if not already cached
    /// 2. Create a container with proper Windows configuration
    /// 3. Use containerd-shim-runhcs-v1 as the runtime
    pub async fn create(
        &self,
        id: &str,
        config: &SandboxConfig,
        _bundle_path: Option<&Path>,
    ) -> Result<()> {
        info!("Creating Windows container via containerd gRPC: {}", id);

        // Ensure image is available
        if !self.image_exists(&config.image).await {
            self.pull_image(&config.image).await?;
        }

        // Generate OCI spec for Windows
        let spec = generate_windows_oci_config_for_containerd(config, self.isolation);
        let spec_bytes = serde_json::to_vec(&spec).map_err(|e| {
            Error::SandboxCreationFailed(format!("Failed to serialize OCI spec: {}", e))
        })?;

        let spec_any = Any {
            type_url: "types.containerd.io/opencontainers/runtime-spec/1/Spec".to_string(),
            value: spec_bytes,
        };

        // Create container
        let mut containers_client = ContainersClient::new(self.channel.clone());

        let container = Container {
            id: id.to_string(),
            image: config.image.clone(),
            snapshotter: "windows".to_string(),
            snapshot_key: id.to_string(),
            runtime: Some(containerd_client::services::v1::container::Runtime {
                name: RUNHCS_RUNTIME.to_string(),
                options: None,
            }),
            spec: Some(spec_any),
            ..Default::default()
        };

        let request = CreateContainerRequest {
            container: Some(container),
        };

        containers_client
            .create(with_namespace!(request, &self.namespace))
            .await
            .map_err(|e| {
                Error::SandboxCreationFailed(format!("Failed to create container: {}", e))
            })?;

        debug!("Container created: {}", id);

        // Create task with stdin/stdout/stderr
        let tmp_dir = std::env::temp_dir().join("nanosandbox").join(id);
        std::fs::create_dir_all(&tmp_dir)?;

        let stdin_path = tmp_dir.join("stdin");
        let stdout_path = tmp_dir.join("stdout");
        let stderr_path = tmp_dir.join("stderr");

        // Create the files
        std::fs::File::create(&stdin_path)?;
        std::fs::File::create(&stdout_path)?;
        std::fs::File::create(&stderr_path)?;

        // Get mounts from snapshotter for the container's rootfs
        let mut snapshots_client = SnapshotsClient::new(self.channel.clone());
        let mounts_request = MountsRequest {
            snapshotter: "windows".to_string(),
            key: id.to_string(),
        };

        let mounts_response: tonic::Response<MountsResponse> = snapshots_client
            .mounts(with_namespace!(mounts_request, &self.namespace))
            .await
            .map_err(|e| {
                Error::SandboxCreationFailed(format!("Failed to get snapshot mounts: {}", e))
            })?;

        let rootfs_mounts = mounts_response.into_inner().mounts;
        debug!(
            "Got {} mounts from snapshotter for container {}",
            rootfs_mounts.len(),
            id
        );

        let mut tasks_client = TasksClient::new(self.channel.clone());

        let task_request = CreateTaskRequest {
            container_id: id.to_string(),
            rootfs: rootfs_mounts,
            stdin: stdin_path.to_string_lossy().to_string(),
            stdout: stdout_path.to_string_lossy().to_string(),
            stderr: stderr_path.to_string_lossy().to_string(),
            ..Default::default()
        };

        tasks_client
            .create(with_namespace!(task_request, &self.namespace))
            .await
            .map_err(|e| Error::SandboxCreationFailed(format!("Failed to create task: {}", e)))?;

        debug!("Task created for container: {}", id);

        // Start the task
        let start_request = StartRequest {
            container_id: id.to_string(),
            ..Default::default()
        };

        tasks_client
            .start(with_namespace!(start_request, &self.namespace))
            .await
            .map_err(|e| Error::SandboxCreationFailed(format!("Failed to start task: {}", e)))?;

        // Save container metadata
        self.save_container_metadata(id, config)?;

        info!("Created and started Windows container: {}", id);
        Ok(())
    }

    /// Start a Windows container
    ///
    /// Note: With the gRPC flow, the task is already started during create.
    /// This method is provided for API compatibility.
    pub async fn start(&self, id: &str) -> Result<()> {
        debug!(
            "Container {} should already be running (started during create)",
            id
        );

        // Verify container task exists
        let running = self.container_running(id).await;
        if !running {
            // Try to start it
            let mut tasks_client = TasksClient::new(self.channel.clone());

            let start_request = StartRequest {
                container_id: id.to_string(),
                ..Default::default()
            };

            match tasks_client
                .start(with_namespace!(start_request, &self.namespace))
                .await
            {
                Ok(_) => debug!("Task started for container: {}", id),
                Err(e) => debug!("Task start returned (may be already running): {}", e),
            }
        }

        info!("Started Windows container: {}", id);
        Ok(())
    }

    /// Check if a container task is running
    async fn container_running(&self, id: &str) -> bool {
        use containerd_client::services::v1::{tasks_client::TasksClient, ListTasksRequest};

        let mut client = TasksClient::new(self.channel.clone());

        let request = ListTasksRequest {
            filter: format!("id=={}", id),
        };

        match client
            .list(with_namespace!(request, &self.namespace))
            .await
        {
            Ok(response) => {
                let tasks = &response.get_ref().tasks;
                tasks.iter().any(|t| {
                    t.id == id
                        && t.status
                            == containerd_client::types::v1::Status::Running as i32
                })
            }
            Err(_) => false,
        }
    }

    /// Execute a command in a running Windows container
    ///
    /// Note: containerd's TasksClient doesn't have a direct exec method.
    /// We fall back to using the ctr CLI for exec operations as it properly
    /// handles the exec process lifecycle through the shim.
    pub async fn exec(
        &self,
        id: &str,
        command: &str,
        args: &[&str],
        workdir: Option<&str>,
        env: &HashMap<String, String>,
    ) -> Result<super::ExecOutput> {
        use tokio::process::Command;

        let exec_id = format!("{}-exec-{}", id, uuid::Uuid::new_v4());

        let mut exec_args = vec![
            "--address".to_string(),
            self.address.clone(),
            "--namespace".to_string(),
            self.namespace.clone(),
            "tasks".to_string(),
            "exec".to_string(),
            "--exec-id".to_string(),
            exec_id,
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

        debug!("Running exec via ctr: {:?}", exec_args);

        let output = Command::new("ctr")
            .args(&exec_args)
            .output()
            .await
            .map_err(|e| Error::ExecFailed(format!("Failed to run ctr exec: {}", e)))?;

        Ok(super::ExecOutput {
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
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::process::Command;

        let exec_id = format!("{}-exec-{}", id, uuid::Uuid::new_v4());

        let mut exec_args = vec![
            "--address".to_string(),
            self.address.clone(),
            "--namespace".to_string(),
            self.namespace.clone(),
            "tasks".to_string(),
            "exec".to_string(),
            "--exec-id".to_string(),
            exec_id,
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

        debug!("Running (streaming) exec via ctr: {:?}", exec_args);

        let mut child = Command::new("ctr")
            .args(&exec_args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| Error::ExecFailed(format!("Failed to spawn ctr exec: {}", e)))?;

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

        // Drain remaining stderr
        while let Ok(Some(text)) = stderr_reader.next_line().await {
            on_output(&text, true);
        }

        let status = child.wait().await?;
        Ok(status.code().unwrap_or(-1))
    }

    /// Stop a Windows container via gRPC
    pub async fn stop(&self, id: &str) -> Result<()> {
        debug!("Stopping Windows container: {}", id);

        let mut tasks_client = TasksClient::new(self.channel.clone());

        // Send SIGTERM first
        let kill_request = KillRequest {
            container_id: id.to_string(),
            signal: 15, // SIGTERM
            ..Default::default()
        };

        match tasks_client
            .kill(with_namespace!(kill_request, &self.namespace))
            .await
        {
            Ok(_) => debug!("Sent SIGTERM to container: {}", id),
            Err(e) => debug!("Kill returned (may be expected): {}", e),
        }

        // Wait a moment for graceful shutdown
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Force kill if still running
        if self.container_running(id).await {
            let kill_request = KillRequest {
                container_id: id.to_string(),
                signal: 9, // SIGKILL
                ..Default::default()
            };

            let _ = tasks_client
                .kill(with_namespace!(kill_request, &self.namespace))
                .await;
        }

        Ok(())
    }

    /// Destroy a Windows container via gRPC
    pub async fn destroy(&self, id: &str) -> Result<()> {
        debug!("Destroying Windows container: {}", id);

        // Stop the task first
        self.stop(id).await?;

        // Delete the task
        let mut tasks_client = TasksClient::new(self.channel.clone());

        let delete_task_request = DeleteTaskRequest {
            container_id: id.to_string(),
        };

        match tasks_client
            .delete(with_namespace!(delete_task_request, &self.namespace))
            .await
        {
            Ok(_) => debug!("Task deleted for container: {}", id),
            Err(e) => debug!("Task delete returned (may not exist): {}", e),
        }

        // Delete the container
        let mut containers_client = ContainersClient::new(self.channel.clone());

        let delete_container_request = DeleteContainerRequest {
            id: id.to_string(),
        };

        match containers_client
            .delete(with_namespace!(delete_container_request, &self.namespace))
            .await
        {
            Ok(_) => debug!("Container deleted: {}", id),
            Err(e) => {
                if !e.message().contains("not found") {
                    warn!("Container delete returned: {}", e);
                }
            }
        }

        // Clean up temp files
        let tmp_dir = std::env::temp_dir().join("nanosandbox").join(id);
        let _ = std::fs::remove_dir_all(tmp_dir);

        // Clean up metadata
        self.remove_container_metadata(id);

        info!("Destroyed Windows container: {}", id);
        Ok(())
    }

    /// Save container metadata to state directory
    fn save_container_metadata(&self, id: &str, config: &SandboxConfig) -> Result<()> {
        let metadata_path = self.state_dir.join(format!("{}.json", id));
        let metadata = serde_json::json!({
            "id": id,
            "image": config.image,
            "created_at": chrono::Utc::now().to_rfc3339(),
        });
        std::fs::write(&metadata_path, serde_json::to_string_pretty(&metadata)?)?;
        Ok(())
    }

    /// Remove container metadata
    fn remove_container_metadata(&self, id: &str) {
        let metadata_path = self.state_dir.join(format!("{}.json", id));
        let _ = std::fs::remove_file(metadata_path);
    }
}

/// Generate Windows-specific OCI runtime configuration for containerd
///
/// Unlike direct runhcs usage, this does NOT include `layerFolders` because
/// containerd's snapshotter handles rootfs mounting.
pub fn generate_windows_oci_config_for_containerd(
    config: &SandboxConfig,
    isolation: WindowsContainerdIsolation,
) -> serde_json::Value {
    use serde_json::json;

    let env: Vec<String> = config
        .env
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .chain(default_windows_env())
        .collect();

    let mut windows_config = json!({
        "resources": {
            "memory": {
                "limit": (config.memory_mb as u64) * 1024 * 1024
            },
            "cpu": {
                "count": config.cpus as u64
            }
        },
        "network": {
            "endpointList": []
        }
    });

    // Add Hyper-V configuration if using Hyper-V isolation
    if isolation == WindowsContainerdIsolation::HyperV {
        windows_config["hyperv"] = json!({});
    }

    json!({
        "ociVersion": "1.0.2",
        "process": {
            "terminal": false,
            "user": {
                "username": "ContainerUser"
            },
            "args": ["cmd.exe", "/c", "echo ready && ping -n 999999 127.0.0.1 > nul"],
            "env": env,
            "cwd": config.workdir.replace('/', "\\"),
        },
        "root": {
            "path": "rootfs",
        },
        "hostname": config.name.chars().take(15).collect::<String>(),
        "windows": windows_config
    })
}

/// Default environment variables for Windows containers
fn default_windows_env() -> Vec<String> {
    vec![
        "PATH=C:\\Windows\\system32;C:\\Windows;C:\\Windows\\System32\\Wbem;C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\".to_string(),
        "PATHEXT=.COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC".to_string(),
        "COMPUTERNAME=SANDBOX".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_isolation_default() {
        assert_eq!(
            WindowsContainerdIsolation::default(),
            WindowsContainerdIsolation::Process
        );
    }

    #[test]
    fn test_generate_windows_oci_config_for_containerd() {
        let config = SandboxConfig::builder()
            .name("test-sandbox")
            .image("mcr.microsoft.com/windows/nanoserver:ltsc2022")
            .cpus(2)
            .memory_mb(2048)
            .build();

        let oci_config =
            generate_windows_oci_config_for_containerd(&config, WindowsContainerdIsolation::Process);

        assert_eq!(oci_config["ociVersion"], "1.0.2");
        assert!(
            oci_config["windows"]["resources"]["memory"]["limit"]
                .as_u64()
                .unwrap()
                > 0
        );
        // CPU count should be set
        assert_eq!(
            oci_config["windows"]["resources"]["cpu"]["count"]
                .as_u64()
                .unwrap(),
            2
        );
        // No layerFolders for containerd - snapshotter handles it
        assert!(oci_config["windows"]["layerFolders"].is_null());
        assert!(oci_config["windows"]["hyperv"].is_null());
    }

    #[test]
    fn test_generate_windows_oci_config_hyperv() {
        let config = SandboxConfig::builder()
            .name("test-sandbox")
            .image("mcr.microsoft.com/windows/nanoserver:ltsc2022")
            .cpus(2)
            .memory_mb(2048)
            .build();

        let oci_config =
            generate_windows_oci_config_for_containerd(&config, WindowsContainerdIsolation::HyperV);

        // Hyper-V config should be present
        assert!(oci_config["windows"]["hyperv"].is_object());
    }

    #[test]
    fn test_default_windows_env() {
        let env = default_windows_env();
        assert!(env.iter().any(|e| e.starts_with("PATH=")));
        assert!(env.iter().any(|e| e.starts_with("PATHEXT=")));
    }
}
