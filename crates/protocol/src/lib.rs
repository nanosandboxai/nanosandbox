//! # nanosb-protocol
//!
//! The versioned control protocol shared by the `nanosb-runtime` helper and
//! every language SDK.
//!
//! - **Transport:** newline-delimited JSON (one compact message per line) over
//!   a Unix stream socket. See [`frame`].
//! - **Handshake:** every connection opens with [`hello::HelloRequest`]; the
//!   helper answers with [`hello::HelloResponse`] carrying the negotiated
//!   protocol version and its [`hello::Capability`] set.
//! - **Verbs:** [`verbs::Request`] / [`verbs::Response`].
//! - **Wire safety:** the `type` tags in `Request`/`Response` are a public ABI
//!   and are pinned by the tests in this crate.

#![forbid(unsafe_code)]
#![allow(missing_docs)]
#![warn(clippy::all)]

pub mod base64;
pub mod error;
pub mod frame;
pub mod guest_fs;
pub mod hello;
pub mod types;
pub mod verbs;

pub use base64::{decode as base64_decode, encode as base64_encode};
pub use error::{ErrorCode, ProtocolError};
pub use frame::{read_message, write_message};
pub use guest_fs::{FsRequest, FsResponse};
pub use hello::{cmp_version, version_in_range, Capability, HelloRequest, HelloResponse};
pub use types::*;
pub use verbs::*;

/// The protocol version implemented by this crate.
///
/// Bumped only on a breaking wire change. Additive verbs do not bump it; they
/// are gated by [`Capability`] instead.
pub const PROTOCOL_VERSION: &str = "1.0.0";

/// Human-readable name of this SDK/client implementation.
pub const CLIENT_NAME: &str = "nanosb-client";

/// The socket file name, relative to a sandbox's state directory.
pub const CONTROL_SOCKET_FILE: &str = "control.sock";

/// The per-sandbox exec bridge socket file name.
pub const EXEC_SOCKET_FILE: &str = "exec.sock";

/// The global helper daemon socket file name, relative to `$HOME/.nanosandbox`.
pub const HELPER_SOCKET_FILE: &str = "helper.sock";

/// The state directory (`state.json`) file name.
pub const STATE_FILE: &str = "state.json";
