//! Nanosandbox - VM-based sandbox SDK
//!
//! Nanosandbox provides hardware-isolated execution environments using
//! [libkrun](https://github.com/containers/libkrun) and
//! [crun](https://github.com/containers/crun).
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
//! ```ignore
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

pub mod config;
pub mod error;
pub mod image;
pub mod runtime;
pub mod sandbox;

// Re-exports
pub use config::SandboxConfig;
pub use error::{Error, Result};
pub use sandbox::Sandbox;

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::config::*;
    pub use crate::error::{Error, Result};
    pub use crate::sandbox::*;
}
