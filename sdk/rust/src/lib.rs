//! # nanosandbox-sdk (Rust SDK)
//!
//! The Rust SDK for the nanosandbox protocol helper. This crate re-exports the
//! reference [`nanosb_client`] surface so downstream crates depend on a single
//! stable `nanosandbox-sdk` name, mirroring the other language SDKs, and adds
//! [`ManagedRuntime`] to locate, provision, and spawn the `nanosb-runtime`
//! helper so managing real microVMs needs no extra setup.
//!
//! ```no_run
//! use nanosandbox_sdk::{SandboxBuilder, SandboxClient};
//!
//! let client = SandboxClient::local().unwrap();
//! let sandbox = client
//!     .create_with(SandboxBuilder::new("alpine:latest").name("demo"))
//!     .unwrap();
//! let out = sandbox.exec("echo", &["hi"]).unwrap();
//! assert_eq!(out.stdout, "hi\n");
//! ```
//!
//! To let the SDK start (and provision) the helper for you:
//!
//! ```no_run
//! use nanosandbox_sdk::{ManagedRuntime, SandboxBuilder, SandboxClient};
//!
//! let rt = ManagedRuntime::start().unwrap();
//! let client = SandboxClient::connect(rt.socket()).unwrap();
//! let sandbox = client
//!     .create_with(SandboxBuilder::new("alpine:latest").name("demo"))
//!     .unwrap();
//! let out = sandbox.exec("echo", &["hi"]).unwrap();
//! assert_eq!(out.stdout, "hi\n");
//! drop(rt);
//! ```

#![forbid(unsafe_code)]
#![allow(missing_docs)]
#![warn(clippy::all)]

pub mod managed_runtime;

pub use managed_runtime::{ManagedRuntime, RuntimeError};
pub use nanosb_client::*;
