//! Nanosandbox - VM-based sandbox SDK
//!
//! Nanosandbox provides hardware-isolated execution environments using
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
//! use nanosandbox::{Sandbox, SandboxConfig};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let config = SandboxConfig::builder()
//!         .name("my-sandbox")
//!         .image("python:3.12-slim")
//!         .cpus(2)
//!         .memory_mb(4096)
//!         .build();
//!
//!     let mut sandbox = Sandbox::create(config).await?;
//!     sandbox.start().await?;
//!
//!     let result = sandbox.exec("python", &["-c", "print('Hello!')"]).await?;
//!     println!("Output: {}", result.stdout);
//!
//!     sandbox.destroy().await?;
//!     Ok(())
//! }
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]

pub mod auth;
pub mod config;
pub mod error;
pub mod image;
pub mod oci;
pub mod registry;
pub mod runtime;
pub mod sandbox;

// Re-exports
pub use auth::CredentialStore;
pub use config::{
    Mount, MountType, NetworkConfig, NetworkMode, NetworkScope, PortMapping, RegistryConfig,
    SandboxConfig,
};
pub use error::{Error, Result};
pub use image::{ImageManager, ImageRef, PulledImage};
pub use oci::OciBundle;
pub use registry::{SandboxInfo, SandboxRegistry};
pub use runtime::{detect_runtime, Runtime};
pub use sandbox::{ExecOptions, ExecResult, OutputChunk, Sandbox, SandboxStatus, Stream};

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
