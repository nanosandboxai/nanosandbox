//! Sandbox wrapper that composes runtime::Sandbox with project-mount management.
//!
//! The `runtime` crate provides a pure microVM runtime with no knowledge of
//! project mounts, gateway helpers, or agent sessions. This `Sandbox` wraps
//! `runtime::Sandbox` and adds the sandbox-layer concerns, starting with
//! project-mount lifecycle (git clone + auto-commit on teardown).

use crate::project::{BranchStrategy, ProjectMount};
use runtime::{ProgressFn, Result, SandboxConfig};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use tracing::{error, info};

/// Agent-layer sandbox: a `runtime::Sandbox` plus a project mount.
///
/// Derefs to `runtime::Sandbox` so all VM lifecycle methods (`start`, `stop`,
/// `exec`, etc.) are available directly. `create` / `create_with_manager` set
/// up the project mount before VM creation; `destroy` tears it down after.
pub struct Sandbox {
    inner: runtime::Sandbox,
    project_mount: Option<ProjectMount>,
}

impl Sandbox {
    /// Create a new sandbox from configuration.
    ///
    /// If `config.project` is set, a `ProjectMount` is detected and set up
    /// before the VM is created; the resulting virtiofs mount is added to
    /// `config.mounts` so it appears in the OCI config.
    pub async fn create(config: SandboxConfig) -> Result<Self> {
        Self::create_with_progress(config, None).await
    }

    /// Create a new sandbox with a progress callback.
    pub async fn create_with_progress(
        mut config: SandboxConfig,
        on_progress: Option<&ProgressFn>,
    ) -> Result<Self> {
        let project_mount = setup_project_mount(&mut config, on_progress)?;
        let inner = runtime::Sandbox::create_with_progress(config, on_progress).await?;
        Ok(Self {
            inner,
            project_mount,
        })
    }

    /// Create a sandbox with a pre-existing image manager.
    pub async fn create_with_manager(
        config: SandboxConfig,
        image_manager: Arc<runtime::ImageManager>,
    ) -> Result<Self> {
        Self::create_with_manager_progress(config, image_manager, None).await
    }

    /// Create a sandbox with a pre-existing image manager and progress callback.
    pub async fn create_with_manager_progress(
        mut config: SandboxConfig,
        image_manager: Arc<runtime::ImageManager>,
        on_progress: Option<&ProgressFn>,
    ) -> Result<Self> {
        let project_mount = setup_project_mount(&mut config, on_progress)?;
        let inner = runtime::Sandbox::create_with_manager_progress(
            config,
            image_manager,
            on_progress,
        )
        .await?;
        Ok(Self {
            inner,
            project_mount,
        })
    }

    /// Wrap an already-created runtime::Sandbox without a project mount.
    ///
    /// Useful when the caller manages the project mount externally (e.g. the
    /// CLI's TUI resume flow, where the mount lives on a panel).
    pub fn from_runtime(inner: runtime::Sandbox) -> Self {
        Self {
            inner,
            project_mount: None,
        }
    }

    /// Get a reference to the inner runtime sandbox.
    pub fn runtime_sandbox(&self) -> &runtime::Sandbox {
        &self.inner
    }

    /// Get a reference to the active project mount, if any.
    pub fn project_mount(&self) -> Option<&ProjectMount> {
        self.project_mount.as_ref()
    }

    /// Get a mutable reference to the active project mount, if any.
    pub fn project_mount_mut(&mut self) -> Option<&mut ProjectMount> {
        self.project_mount.as_mut()
    }

    /// Take ownership of the project mount (for transferring to TUI panels).
    pub fn take_project_mount(&mut self) -> Option<ProjectMount> {
        self.project_mount.take()
    }

    /// Start the sandbox.
    ///
    /// Before booting the VM, detects gateway mode from the rootfs and sets
    /// `config.command` and `config.gateway` so the runtime knows what PID 1
    /// to run and whether to set up gateway infrastructure.
    pub async fn start(&mut self) -> Result<()> {
        // Detect gateway mode from the bundle rootfs.
        if let Some(bundle_path) = self.inner.bundle_path() {
            let rootfs = bundle_path.join("rootfs");
            let has_init = rootfs.join("usr/local/bin/nanosb-init.sh").exists()
                || rootfs.join("usr/local/bin/agent-gateway").exists()
                || rootfs.join(".nanosb-layers").exists();

            if has_init {
                self.inner.config_mut().command =
                    Some("/usr/local/bin/nanosb-init.sh".to_string());
                self.inner.config_mut().command_args = vec![];
                self.inner.config_mut().gateway = true;
            }
        }

        self.inner.start().await
    }

    /// Destroy the sandbox: tear down project mount, then destroy the VM.
    pub async fn destroy(mut self) -> Result<()> {
        // Teardown project mount (auto-commit and remove clone) before VM destroy.
        if let Some(mut pm) = self.project_mount.take() {
            if let Err(e) = pm.teardown() {
                tracing::warn!("Failed to teardown project mount: {}", e);
            }
        }
        self.inner.destroy().await
    }
}

impl Deref for Sandbox {
    type Target = runtime::Sandbox;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Sandbox {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Detect and set up the project mount declared by `config.project`.
///
/// Mutates `config.mounts` to include the virtiofs mount. Returns the
/// `ProjectMount` so the caller can manage its lifecycle (teardown on destroy).
pub fn setup_project_mount(
    config: &mut SandboxConfig,
    on_progress: Option<&ProgressFn>,
) -> Result<Option<ProjectMount>> {
    let Some(proj_config) = config.project.clone() else {
        return Ok(None);
    };

    if let Some(cb) = on_progress {
        cb("Setting up project...");
    }

    // Generate a per-mount id independent of the VM's uuid — ProjectMount uses
    // this purely for branch/clone naming and doesn't need to match sandbox.id.
    let id = uuid::Uuid::new_v4().to_string();

    let mut pm = ProjectMount::detect(&proj_config.path).map_err(|e| {
        error!(mount_id = %id, "Project detection failed: {}", e);
        runtime::Error::SandboxCreationFailed(format!("Project detection failed: {}", e))
    })?;

    let strategy = match &proj_config.branch {
        Some(name) => BranchStrategy::Named(name.clone()),
        None => BranchStrategy::Auto,
    };

    let wt_path = if proj_config.auto_sync {
        pm.setup(&id, &strategy).map_err(|e| {
            error!(mount_id = %id, "Project clone setup failed: {}", e);
            runtime::Error::SandboxCreationFailed(format!("Project clone setup failed: {}", e))
        })?
    } else {
        pm.setup_deferred(&id, &strategy).map_err(|e| {
            error!(mount_id = %id, "Project clone setup (deferred) failed: {}", e);
            runtime::Error::SandboxCreationFailed(format!(
                "Project clone setup (deferred) failed: {}",
                e
            ))
        })?
    };

    info!(
        "Project mounted: {} -> {}",
        wt_path.display(),
        proj_config.mount_point
    );

    if let Some(mount) = pm.mount_config(&proj_config.mount_point) {
        config.mounts.push(mount);
    }

    Ok(Some(pm))
}

