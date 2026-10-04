//! Gateway crate — agent-gateway client for communicating with in-VM gateway processes.
//!
//! This crate encapsulates all communication with the agent-gateway HTTP server
//! running inside VMs: transport (TCP/HvSocket), command execution, health checks,
//! secrets injection, and SSH key management.

pub mod client;
pub mod error;
pub mod http;
#[cfg(target_os = "windows")]
pub mod hvsocket;
pub mod secrets;
pub mod ssh;
pub mod transport;

pub use client::{ExecOptions, ExecResult, GatewayClient, OutputChunk, Stream};
pub use error::{Error, Result};
pub use secrets::{
    encrypt_payload, generate_gateway_keypair, EncryptedPayload, GatewayKeyPair, SecretManifest,
    SecretPayload,
};
pub use ssh::{cleanup_ssh_keys, generate_ssh_keys, inject_pubkey_into_rootfs};
pub use transport::Transport;
