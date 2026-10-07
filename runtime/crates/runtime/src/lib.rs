//! Runtime - VM-based sandbox engine
//!
//! Runtime provides hardware-isolated execution environments using
//! [libkrun](https://github.com/containers/libkrun) via direct FFI.
//!
//! # Features
//!
//! - **VM-Level Isolation**: Each sandbox runs in its own microVM
//! - **OCI Image Support**: Use any container image from any registry
//! - **Fast Boot Times**: Sub-second VM startup
//! - **Cross-Platform**: Linux (KVM) and macOS Apple Silicon (HVF)
//!
//! # Example
//!
//! ```no_run
//! use runtime::{Sandbox, SandboxConfig};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let config = SandboxConfig::builder()
//!         .name("my-sandbox")
//!         .image("python:3.12-slim")
//!         .cpus(1)
//!         .memory_mb(512)
//!         .build();
//!
//!     let mut sandbox = Sandbox::create(config).await?;
//!     sandbox.start().await?;
//!
//!     // Command execution and logs are handled by the sandbox supervisor
//!     // via the virtio-console channel.
//!
//!     sandbox.destroy().await?;
//!     Ok(())
//! }
//! ```

// Missing docs: warn locally but don't block CI (tracked separately)
#![allow(missing_docs)]
#![warn(clippy::all)]

pub mod auth;
pub mod config;
pub mod error;
pub mod exec;
pub mod image;
pub mod oci;
pub mod registry;
pub mod runtime;
pub mod sandbox;

// Re-exports
pub use auth::CredentialStore;
pub use config::{
    ConsoleSpec, ExtraMount, Mount, MountType, NetworkConfig, NetworkMode, NetworkScope,
    PortMapping, ProjectConfig, RegistryConfig, RuntimeMode, SandboxConfig,
};
pub use error::{Error, Result};
pub use image::{ImageManager, ImageRef, PruneResult, PulledImage};
pub use oci::OciBundle;
pub use registry::{SandboxInfo, SandboxRegistry};
pub use runtime::{detect_runtime, Runtime};
pub use sandbox::{ProgressFn, Sandbox, SandboxStatus};

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::auth::CredentialStore;
    pub use crate::config::*;
    pub use crate::error::{Error, Result};
    pub use crate::image::{ImageManager, ImageRef, PulledImage};
    pub use crate::oci::OciBundle;
    pub use crate::registry::{SandboxInfo, SandboxRegistry};
    pub use crate::runtime::Runtime;
    pub use crate::sandbox::*;
}
