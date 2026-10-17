//! # nanosandbox (Rust SDK)
//!
//! The Rust SDK for the nanosandbox protocol helper. This crate re-exports the
//! reference [`nanosb_client`] surface so downstream crates depend on a single
//! stable `nanosandbox` name, mirroring the other language SDKs.
//!
//! ```no_run
//! use nanosandbox::{SandboxBuilder, SandboxClient};
//!
//! let client = SandboxClient::local().unwrap();
//! let sandbox = client
//!     .create_with(SandboxBuilder::new("alpine:latest").name("demo"))
//!     .unwrap();
//! let out = sandbox.exec("echo", &["hi"]).unwrap();
//! assert_eq!(out.stdout, "hi\n");
//! ```

#![forbid(unsafe_code)]
#![allow(missing_docs)]
#![warn(clippy::all)]

pub use nanosb_client::*;
