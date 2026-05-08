//! Cryptographic operations for gateway secrets.
//!
//! Re-exports from the gateway crate.

pub use gateway::secrets::{
    encrypt_payload, generate_gateway_keypair, EncryptedPayload, GatewayKeyPair,
};
